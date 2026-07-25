//! Bounded subprocess execution for the external Z3 solver.

use std::{
    env,
    ffi::OsStr,
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::OnceLock,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{self, Instant},
};

/// Fixed Z3 soft memory cap, in MiB.
pub const Z3_MEMORY_LIMIT_MB: u32 = 1_024;
/// Maximum captured Z3 stdout size.
pub const MAX_STDOUT_BYTES: usize = 1024 * 1024;
/// Maximum captured Z3 stderr size.
pub const MAX_STDERR_BYTES: usize = 256 * 1024;

/// One invocation passed to a [`ProcessRunner`].
#[derive(Clone, Debug)]
pub struct ProcessRequest {
    smt: String,
    deadline: Instant,
}

impl ProcessRequest {
    /// Creates a subprocess request with an absolute wall-clock deadline.
    #[must_use]
    pub fn new(smt: String, deadline: Instant) -> Self {
        Self { smt, deadline }
    }

    /// Returns the generated SMT-LIB sent to Z3 stdin.
    #[must_use]
    pub fn smt(&self) -> &str {
        &self.smt
    }

    /// Returns the authoritative wall-clock deadline.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// Captured output from a completed solver child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// Whether the child exit status was successful.
    pub success: bool,
    /// Portable numeric exit code, absent when termination was signal-based.
    pub exit_code: Option<i32>,
    /// Bounded stdout bytes.
    pub stdout: Vec<u8>,
    /// Bounded stderr bytes.
    pub stderr: Vec<u8>,
}

/// Terminal result from a solver subprocess.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessOutcome {
    /// The child exited and both output streams were drained.
    Completed(ProcessOutput),
    /// The wall-clock deadline expired and the child tree was killed.
    TimedOut,
}

/// Output stream whose configured byte cap was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl fmt::Display for OutputStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// Failure to discover, spawn, communicate with, or reap the solver process.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// Neither `SPUR_Z3_BIN` nor `PATH` resolved a Z3 executable.
    #[error("{message}")]
    SolverUnavailable {
        /// Operator-facing install/discovery diagnostic.
        message: String,
    },
    /// Z3 could not be spawned.
    #[error("failed to spawn Z3 executable `{binary}`: {source}")]
    Spawn {
        /// Resolved executable path.
        binary: PathBuf,
        /// Operating-system spawn error.
        #[source]
        source: io::Error,
    },
    /// A required child stdio handle was unexpectedly absent.
    #[error("spawned Z3 child did not expose piped {stream}")]
    MissingPipe {
        /// Missing pipe name.
        stream: &'static str,
    },
    /// Writing the generated script to stdin failed.
    #[error("failed to write Z3 stdin: {source}")]
    WriteStdin {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Reading a child output stream failed.
    #[error("failed to read Z3 {stream}: {source}")]
    ReadOutput {
        /// Stream being read.
        stream: OutputStream,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Waiting for the child failed.
    #[error("failed to wait for Z3 child: {source}")]
    Wait {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A stream exceeded its bounded capture size.
    #[error("Z3 {stream} exceeded the {max_bytes}-byte capture limit")]
    OutputTooLarge {
        /// Stream that exceeded its cap.
        stream: OutputStream,
        /// Configured maximum.
        max_bytes: usize,
    },
    /// A Tokio task used to drain stdio failed unexpectedly.
    #[error("Z3 {task} task failed: {message}")]
    Task {
        /// Task role.
        task: &'static str,
        /// Join failure diagnostic.
        message: String,
    },
}

/// Boxed future returned by a [`ProcessRunner`] implementation.
pub type ProcessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProcessOutcome, ProcessError>> + Send + 'a>>;

/// Injectable boundary around solver execution.
///
/// Implementations must honor [`ProcessRequest::deadline`]. Production uses
/// [`Z3Process`]; deterministic service tests may inject a lightweight runner.
pub trait ProcessRunner: Send + Sync + fmt::Debug {
    /// Runs one complete solver invocation.
    fn run(&self, request: ProcessRequest) -> ProcessFuture<'_>;
}

/// Production subprocess runner for `z3 -in`.
///
/// Binary discovery is lazy and cached. Operator configuration is read from
/// `SPUR_Z3_BIN` first, then `PATH`; no solve request can provide a binary or
/// arbitrary command-line arguments.
#[derive(Debug)]
pub struct Z3Process {
    configured_binary: Option<PathBuf>,
    discovered_binary: OnceLock<Result<PathBuf, String>>,
}

impl Z3Process {
    /// Creates a runner that discovers Z3 from operator environment settings.
    ///
    /// # Examples
    ///
    /// ```
    /// use spur_solver::process::Z3Process;
    ///
    /// let runner = Z3Process::new();
    /// assert!(format!("{runner:?}").contains("Z3Process"));
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            configured_binary: None,
            discovered_binary: OnceLock::new(),
        }
    }

    /// Creates a runner pinned to an operator-supplied executable path.
    ///
    /// This constructor is intended for host composition and fake-binary
    /// tests. Agent-facing solve requests never expose it.
    #[must_use]
    pub const fn with_binary(binary: PathBuf) -> Self {
        Self {
            configured_binary: Some(binary),
            discovered_binary: OnceLock::new(),
        }
    }

    fn resolve_binary(&self) -> Result<&Path, ProcessError> {
        if let Some(binary) = self.configured_binary.as_deref() {
            return Ok(binary);
        }

        self.discovered_binary
            .get_or_init(discover_z3_binary)
            .as_deref()
            .map_err(|message| ProcessError::SolverUnavailable {
                message: message.clone(),
            })
    }

    async fn run_inner(&self, request: ProcessRequest) -> Result<ProcessOutcome, ProcessError> {
        if Instant::now() >= request.deadline {
            return Ok(ProcessOutcome::TimedOut);
        }

        let binary = self.resolve_binary()?.to_owned();
        let timeout_seconds = z3_backstop_seconds(request.deadline);
        let mut command = Command::new(&binary);
        command
            .arg("-in")
            .arg(format!("-memory:{Z3_MEMORY_LIMIT_MB}"))
            .arg(format!("-T:{timeout_seconds}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.as_std_mut().process_group(0);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(ProcessError::SolverUnavailable {
                    message: format!(
                        "Z3 solver unavailable at `{}`; install Z3 or set SPUR_Z3_BIN",
                        binary.display()
                    ),
                });
            }
            Err(source) => {
                return Err(ProcessError::Spawn { binary, source });
            }
        };

        let process_id = child.id().ok_or(ProcessError::MissingPipe {
            stream: "process id",
        })?;
        let mut group_guard = ProcessGroupGuard::new(process_id);
        let stdin = child
            .stdin
            .take()
            .ok_or(ProcessError::MissingPipe { stream: "stdin" })?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ProcessError::MissingPipe { stream: "stdout" })?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ProcessError::MissingPipe { stream: "stderr" })?;

        let writer = tokio::spawn(write_stdin(stdin, request.smt));
        let (overflow_tx, mut overflow_rx) = mpsc::unbounded_channel();
        let stdout_reader = tokio::spawn(read_capped(
            stdout,
            OutputStream::Stdout,
            MAX_STDOUT_BYTES,
            overflow_tx.clone(),
        ));
        let stderr_reader = tokio::spawn(read_capped(
            stderr,
            OutputStream::Stderr,
            MAX_STDERR_BYTES,
            overflow_tx,
        ));

        enum WaitRace {
            Exited(io::Result<std::process::ExitStatus>),
            Overflow(OutputStream),
            TimedOut,
        }

        let race = {
            let wait = child.wait();
            tokio::pin!(wait);
            tokio::select! {
                status = &mut wait => WaitRace::Exited(status),
                Some(stream) = overflow_rx.recv() => WaitRace::Overflow(stream),
                () = time::sleep_until(request.deadline) => WaitRace::TimedOut,
            }
        };

        match race {
            WaitRace::Overflow(stream) => {
                kill_and_reap(&mut child, process_id).await;
                abort_io_tasks(writer, stdout_reader, stderr_reader).await;
                group_guard.disarm();
                Err(ProcessError::OutputTooLarge {
                    stream,
                    max_bytes: stream_cap(stream),
                })
            }
            WaitRace::TimedOut => {
                kill_and_reap(&mut child, process_id).await;
                abort_io_tasks(writer, stdout_reader, stderr_reader).await;
                group_guard.disarm();
                Ok(ProcessOutcome::TimedOut)
            }
            WaitRace::Exited(status) => {
                let status = match status {
                    Ok(status) => status,
                    Err(source) => {
                        kill_and_reap(&mut child, process_id).await;
                        abort_io_tasks(writer, stdout_reader, stderr_reader).await;
                        group_guard.disarm();
                        return Err(ProcessError::Wait { source });
                    }
                };
                let remaining =
                    collect_tasks(writer, stdout_reader, stderr_reader, request.deadline).await;
                match remaining {
                    Ok((stdout, stderr)) => {
                        group_guard.disarm();
                        Ok(ProcessOutcome::Completed(ProcessOutput {
                            success: status.success(),
                            exit_code: status.code(),
                            stdout,
                            stderr,
                        }))
                    }
                    Err(CollectError::TimedOut {
                        writer,
                        stdout_reader,
                        stderr_reader,
                    }) => {
                        let _ = kill_process_group(process_id);
                        writer.abort();
                        stdout_reader.abort();
                        stderr_reader.abort();
                        group_guard.disarm();
                        Ok(ProcessOutcome::TimedOut)
                    }
                    Err(CollectError::Process(error)) => {
                        let _ = kill_process_group(process_id);
                        group_guard.disarm();
                        Err(error)
                    }
                }
            }
        }
    }
}

impl Default for Z3Process {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRunner for Z3Process {
    fn run(&self, request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(self.run_inner(request))
    }
}

fn discover_z3_binary() -> Result<PathBuf, String> {
    let configured = env::var_os("SPUR_Z3_BIN");
    let path = env::var_os("PATH");
    discover_z3_binary_from(configured.as_deref(), path.as_deref())
}

fn discover_z3_binary_from(
    configured: Option<&OsStr>,
    path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(configured) = configured.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(configured));
    }

    let path = path.ok_or_else(unavailable_message)?;
    find_binary_on_path(path).ok_or_else(unavailable_message)
}

fn find_binary_on_path(path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path).find_map(|directory| {
        z3_executable_names().iter().find_map(|name| {
            let candidate = directory.join(name);
            is_executable_candidate(&candidate).then_some(candidate)
        })
    })
}

#[cfg(unix)]
fn is_executable_candidate(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    candidate
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_candidate(candidate: &Path) -> bool {
    candidate.is_file()
}

#[cfg(windows)]
const fn z3_executable_names() -> &'static [&'static str] {
    &["z3.exe", "z3"]
}

#[cfg(not(windows))]
const fn z3_executable_names() -> &'static [&'static str] {
    &["z3"]
}

fn unavailable_message() -> String {
    "Z3 solver unavailable: `SPUR_Z3_BIN` is unset and `z3` was not found on PATH; install Z3 and configure SPUR_Z3_BIN if needed".to_owned()
}

fn z3_backstop_seconds(deadline: Instant) -> u64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let milliseconds = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
    milliseconds
        .saturating_add(999)
        .saturating_div(1_000)
        .max(1)
}

async fn write_stdin(mut stdin: ChildStdin, smt: String) -> Result<(), ProcessError> {
    stdin
        .write_all(smt.as_bytes())
        .await
        .map_err(|source| ProcessError::WriteStdin { source })?;
    stdin
        .shutdown()
        .await
        .map_err(|source| ProcessError::WriteStdin { source })
}

async fn read_capped<R>(
    mut reader: R,
    stream: OutputStream,
    max_bytes: usize,
    overflow_tx: mpsc::UnboundedSender<OutputStream>,
) -> Result<Vec<u8>, ProcessError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|source| ProcessError::ReadOutput { stream, source })?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > max_bytes {
            let _ = overflow_tx.send(stream);
            return Err(ProcessError::OutputTooLarge { stream, max_bytes });
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

enum CollectError {
    Process(ProcessError),
    TimedOut {
        writer: JoinHandle<Result<(), ProcessError>>,
        stdout_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
        stderr_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
    },
}

async fn collect_tasks(
    mut writer: JoinHandle<Result<(), ProcessError>>,
    mut stdout_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
    mut stderr_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
    deadline: Instant,
) -> Result<(Vec<u8>, Vec<u8>), CollectError> {
    let joined = time::timeout_at(deadline, async {
        let writer_result = (&mut writer).await;
        let stdout_result = (&mut stdout_reader).await;
        let stderr_result = (&mut stderr_reader).await;
        (writer_result, stdout_result, stderr_result)
    })
    .await;

    let Ok((writer_result, stdout_result, stderr_result)) = joined else {
        return Err(CollectError::TimedOut {
            writer,
            stdout_reader,
            stderr_reader,
        });
    };

    flatten_task(writer_result, "stdin writer")
        .and_then(|result| result)
        .map_err(CollectError::Process)?;
    let stdout = flatten_task(stdout_result, "stdout reader")
        .and_then(|result| result)
        .map_err(CollectError::Process)?;
    let stderr = flatten_task(stderr_result, "stderr reader")
        .and_then(|result| result)
        .map_err(CollectError::Process)?;
    Ok((stdout, stderr))
}

async fn abort_io_tasks(
    writer: JoinHandle<Result<(), ProcessError>>,
    stdout_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
    stderr_reader: JoinHandle<Result<Vec<u8>, ProcessError>>,
) {
    writer.abort();
    stdout_reader.abort();
    stderr_reader.abort();
    let _ = writer.await;
    let _ = stdout_reader.await;
    let _ = stderr_reader.await;
}

fn flatten_task<T>(
    result: Result<Result<T, ProcessError>, tokio::task::JoinError>,
    task: &'static str,
) -> Result<Result<T, ProcessError>, ProcessError> {
    result.map_err(|error| ProcessError::Task {
        task,
        message: error.to_string(),
    })
}

const fn stream_cap(stream: OutputStream) -> usize {
    match stream {
        OutputStream::Stdout => MAX_STDOUT_BYTES,
        OutputStream::Stderr => MAX_STDERR_BYTES,
    }
}

async fn kill_and_reap(child: &mut Child, process_id: u32) {
    let _ = kill_process_group(process_id);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

struct ProcessGroupGuard {
    process_id: Option<u32>,
}

impl ProcessGroupGuard {
    const fn new(process_id: u32) -> Self {
        Self {
            process_id: Some(process_id),
        }
    }

    fn disarm(&mut self) {
        self.process_id = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_id) = self.process_id {
            let _ = kill_process_group(process_id);
        }
    }
}

#[cfg_attr(
    unix,
    expect(
        unsafe_code,
        reason = "libc::kill is the minimal Unix process-group termination boundary"
    )
)]
fn kill_process_group(process_id: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let process_group = i32::try_from(process_id).map_err(|_out_of_range| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "child process id does not fit a Unix pid",
            )
        })?;
        // SAFETY: `process_group` is the positive pid returned by the child
        // spawned with `process_group(0)`. Negating it targets only that
        // isolated process group. ESRCH is a benign already-exited race.
        let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(not(unix))]
    {
        let _ = process_id;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::{discover_z3_binary_from, find_binary_on_path, z3_backstop_seconds};
    use tempfile::TempDir;
    use tokio::time::{Duration, Instant};

    #[test]
    fn path_discovery_finds_z3_without_executing_it() {
        let directory = TempDir::new().expect("create path fixture");
        let binary = directory.path().join(super::z3_executable_names()[0]);
        fs::write(&binary, b"fixture").expect("write path fixture");
        #[cfg(unix)]
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make path fixture executable");

        let resolved = find_binary_on_path(directory.path().as_os_str());

        assert_eq!(resolved.as_deref(), Some(binary.as_path()));
    }

    #[test]
    fn path_discovery_returns_none_when_z3_is_absent() {
        let directory = TempDir::new().expect("create empty path fixture");

        assert!(find_binary_on_path(directory.path().as_os_str()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn path_discovery_skips_non_executable_shadow() {
        let shadow_directory = TempDir::new().expect("create shadow path fixture");
        let valid_directory = TempDir::new().expect("create valid path fixture");
        let executable_name = super::z3_executable_names()[0];
        let shadow = shadow_directory.path().join(executable_name);
        let valid = valid_directory.path().join(executable_name);
        fs::write(&shadow, b"shadow fixture").expect("write shadow fixture");
        fs::write(&valid, b"valid fixture").expect("write valid fixture");
        fs::set_permissions(&shadow, fs::Permissions::from_mode(0o600))
            .expect("make shadow non-executable");
        fs::set_permissions(&valid, fs::Permissions::from_mode(0o700))
            .expect("make valid fixture executable");
        let path = std::env::join_paths([shadow_directory.path(), valid_directory.path()])
            .expect("join fixture PATH");

        let resolved = find_binary_on_path(&path);

        assert_eq!(resolved.as_deref(), Some(valid.as_path()));
    }

    #[test]
    fn configured_binary_takes_precedence_over_path() {
        let directory = TempDir::new().expect("create path fixture");
        let path_binary = directory.path().join(super::z3_executable_names()[0]);
        fs::write(&path_binary, b"path fixture").expect("write path fixture");
        let configured = Path::new("/operator/configured/z3");

        let resolved = discover_z3_binary_from(
            Some(configured.as_os_str()),
            Some(directory.path().as_os_str()),
        )
        .expect("configured binary should resolve");

        assert_eq!(resolved, configured);
    }

    #[test]
    fn z3_backstop_rounds_partial_seconds_up() {
        let deadline = Instant::now() + Duration::from_millis(1_500);

        assert_eq!(z3_backstop_seconds(deadline), 2);
    }
}
