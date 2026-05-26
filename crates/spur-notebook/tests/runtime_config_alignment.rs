//! Regression coverage for the Rust proxy -> Tauri daemon runtime config
//! alignment.
//!
//! `notebook_runtime_config` is a Tauri command backed by `tauri::State`, so an
//! external integration test cannot call it directly without driving a webview.
//! The stable observable contract at this layer is the proxy-spawned daemon's
//! argv: `--mcp-proxy <socket> <flag>` must resolve the CLI flag and forward the
//! matching `--notebook-in-proc-store` or `--no-notebook-in-proc-store` flag to
//! the inner daemon. This test unsets both env fallbacks so the CLI flag is the
//! only source of truth, then reads the daemon process command line.
//!
//! On Linux runners with a display, the test waits for the daemon socket before
//! the final argv assertion. On headless Linux, Tauri cannot initialize GTK
//! before binding the socket; that path still asserts the proxy -> daemon argv
//! contract before the expected GTK startup failure. macOS can opt into the
//! socket wait with `SPUR_NOTEBOOK_TEST_WAIT_SOCKET=1`; the default macOS path
//! uses the same argv contract because raw Cargo-built Tauri binaries do not
//! reliably finish app setup outside a bundle.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;

type TestResult<T> = Result<T, Box<dyn Error>>;

const IN_PROC_FLAG: &str = "--notebook-in-proc-store";
const NO_IN_PROC_FLAG: &str = "--no-notebook-in-proc-store";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn mcp_proxy_forwards_explicit_in_proc_store_flags_to_daemon() -> TestResult<()> {
    if !can_inspect_process_commands() {
        eprintln!("skipping: process command-line inspection is not permitted on this runner");
        return Ok(());
    }

    for expected_flag in [IN_PROC_FLAG, NO_IN_PROC_FLAG] {
        let mut harness = ProxyHarness::spawn(expected_flag)?;
        let command_line = if can_wait_for_tauri_socket() {
            harness.wait_for_ready_daemon_command_line(expected_flag)?
        } else {
            harness.wait_for_spawned_daemon_command_line(expected_flag)?
        };
        let unexpected_flag = if expected_flag == IN_PROC_FLAG {
            NO_IN_PROC_FLAG
        } else {
            IN_PROC_FLAG
        };

        assert!(
            command_line.contains(expected_flag),
            "daemon command line did not contain {expected_flag:?}: {command_line}"
        );
        assert!(
            !command_line.contains(unexpected_flag),
            "daemon command line contained unexpected {unexpected_flag:?}: {command_line}"
        );
    }

    Ok(())
}

struct ProxyHarness {
    child: Option<Child>,
    daemon_pid: Option<u32>,
    socket_path: PathBuf,
    proxy_stderr_path: PathBuf,
    daemon_log_path: PathBuf,
    _temp_dir: tempfile::TempDir,
}

impl ProxyHarness {
    fn spawn(flag: &str) -> TestResult<Self> {
        let temp_dir = tempfile::Builder::new()
            .prefix("sn-rc-")
            .tempdir_in("/tmp")?;
        let socket_path = temp_dir.path().join("notebook.sock");
        let home = temp_dir.path().join("home");
        let config_home = temp_dir.path().join("config");
        let data_home = temp_dir.path().join("data");
        let cache_home = temp_dir.path().join("cache");
        let proxy_stderr_path = temp_dir.path().join("proxy.stderr.log");
        let daemon_log_path = temp_dir.path().join("daemon.log");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(&data_home)?;
        fs::create_dir_all(&cache_home)?;
        let proxy_stderr = fs::File::create(&proxy_stderr_path)?;

        let child = Command::new(spur_notebook_binary()?)
            .arg("--mcp-proxy")
            .arg(&socket_path)
            .arg(flag)
            .env_remove("SPUR_NOTEBOOK_IN_PROC_STORE")
            .env_remove("VITE_SPUR_NOTEBOOK_IN_PROC_STORE")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(proxy_stderr))
            .spawn()?;

        Ok(Self {
            child: Some(child),
            daemon_pid: None,
            socket_path,
            proxy_stderr_path,
            daemon_log_path,
            _temp_dir: temp_dir,
        })
    }

    fn wait_for_ready_daemon_command_line(&mut self, expected_flag: &str) -> TestResult<String> {
        let (pid, _) = wait_for_daemon_process(
            &self.socket_path,
            self.child.as_ref().map(Child::id),
            STARTUP_TIMEOUT,
        )
        .map_err(|error| other(format!("{error}\n{}", self.diagnostics())))?;
        self.daemon_pid = Some(pid);

        wait_for_socket(&self.socket_path, STARTUP_TIMEOUT)
            .map_err(|error| other(format!("{error}\n{}", self.diagnostics())))?;
        let command_line = process_command_line(pid)?;

        self.assert_daemon_command_line(&command_line, expected_flag);
        Ok(command_line)
    }

    fn wait_for_spawned_daemon_command_line(&mut self, expected_flag: &str) -> TestResult<String> {
        let (pid, command_line) = wait_for_daemon_process(
            &self.socket_path,
            self.child.as_ref().map(Child::id),
            STARTUP_TIMEOUT,
        )
        .map_err(|error| other(format!("{error}\n{}", self.diagnostics())))?;
        self.daemon_pid = Some(pid);

        self.assert_daemon_command_line(&command_line, expected_flag);
        Ok(command_line)
    }

    fn assert_daemon_command_line(&self, command_line: &str, expected_flag: &str) {
        assert!(
            command_line.contains("--socket"),
            "daemon command line did not include --socket: {command_line}"
        );
        let socket_path = self.socket_path.to_string_lossy();
        assert!(
            command_line.contains(socket_path.as_ref()),
            "daemon command line did not include temp socket {}: {command_line}",
            self.socket_path.display()
        );
        assert!(
            command_line.contains(expected_flag),
            "daemon command line did not include expected flag {expected_flag}: {command_line}"
        );
    }

    fn diagnostics(&mut self) -> String {
        let proxy_status = match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => format!("proxy exited with {status}"),
                Ok(None) => "proxy still running".to_string(),
                Err(error) => format!("failed to poll proxy status: {error}"),
            },
            None => "proxy already reaped".to_string(),
        };
        let daemon_status =
            match find_daemon_pid(&self.socket_path, self.child.as_ref().map(Child::id)) {
                Ok(Some(pid)) => match process_command_line(pid) {
                    Ok(command_line) => format!("daemon pid {pid}: {command_line}"),
                    Err(error) => format!("daemon pid {pid}, command line unavailable: {error}"),
                },
                Ok(None) => "daemon process not found".to_string(),
                Err(error) => format!("daemon process lookup failed: {error}"),
            };
        format!(
            "diagnostics: {proxy_status}; {daemon_status}\nproxy stderr:\n{}\ndaemon log:\n{}",
            read_lossy(&self.proxy_stderr_path),
            read_lossy(&self.daemon_log_path)
        )
    }
}

impl Drop for ProxyHarness {
    fn drop(&mut self) {
        let daemon_pid = self.daemon_pid.or_else(|| {
            find_daemon_pid(&self.socket_path, self.child.as_ref().map(Child::id))
                .ok()
                .flatten()
        });
        if let Some(pid) = daemon_pid {
            terminate_pid(pid);
        }

        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        if let Some(pid) = daemon_pid {
            terminate_pid(pid);
        }

        let _ = fs::remove_file(&self.socket_path);
    }
}

fn spur_notebook_binary() -> TestResult<PathBuf> {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    if let Some(binary) = BINARY.get() {
        return Ok(binary.clone());
    }

    let binary = locate_spur_notebook_binary()?;
    let _ = BINARY.set(binary.clone());
    Ok(binary)
}

fn locate_spur_notebook_binary() -> TestResult<PathBuf> {
    let cargo_bin = PathBuf::from(env!("CARGO_BIN_EXE_spur-notebook"));
    if cargo_bin.exists() {
        return Ok(cargo_bin);
    }

    let workspace_root = workspace_root();
    let status = Command::new(workspace_root.join("scripts/spur-cargo"))
        .args(["build", "-p", "spur-notebook", "--bin", "spur-notebook"])
        .current_dir(&workspace_root)
        .env("SPUR_REMOTE", "0")
        .status()?;
    if !status.success() {
        return Err(other(format!(
            "failed to build spur-notebook fallback binary with status {status}"
        )));
    }

    let binary = fallback_binary_path()?;
    if binary.exists() {
        Ok(binary)
    } else {
        Err(other(format!(
            "built spur-notebook but binary was not found at {}",
            binary.display()
        )))
    }
}

fn can_wait_for_tauri_socket() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("SPUR_NOTEBOOK_TEST_WAIT_SOCKET").is_some()
    }
}

#[cfg(target_os = "linux")]
fn can_inspect_process_commands() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn can_inspect_process_commands() -> bool {
    Command::new("ps")
        .args(["-o", "command=", "-p", &std::process::id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fallback_binary_path() -> TestResult<PathBuf> {
    let mut path = std::env::current_exe()?;
    path.pop();
    if path.file_name().and_then(|name| name.to_str()) == Some("deps") {
        path.pop();
    }
    path.push(if cfg!(windows) {
        "spur-notebook.exe"
    } else {
        "spur-notebook"
    });
    Ok(path)
}

fn wait_for_socket(socket_path: &Path, timeout: Duration) -> TestResult<()> {
    let deadline = Instant::now() + timeout;

    loop {
        let error = match UnixStream::connect(socket_path) {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };

        if Instant::now() >= deadline {
            return Err(other(format!(
                "notebook daemon socket {} did not become connectable within {:?}; last error: {}",
                socket_path.display(),
                timeout,
                error
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_daemon_process(
    socket_path: &Path,
    proxy_pid: Option<u32>,
    timeout: Duration,
) -> TestResult<(u32, String)> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(pid) = find_daemon_pid(socket_path, proxy_pid)? {
            return Ok((pid, process_command_line(pid)?));
        }

        if Instant::now() >= deadline {
            return Err(other(format!(
                "no spur-notebook daemon process with --socket {} appeared within {:?}",
                socket_path.display(),
                timeout
            )));
        }

        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn find_daemon_pid(socket_path: &Path, proxy_pid: Option<u32>) -> TestResult<Option<u32>> {
    if let Some(proxy_pid) = proxy_pid {
        for pid in child_pids(proxy_pid)? {
            if process_args_match_socket(pid, socket_path) {
                return Ok(Some(pid));
            }
        }
    }

    find_daemon_pid_by_proc_scan(socket_path)
}

#[cfg(target_os = "linux")]
fn find_daemon_pid_by_proc_scan(socket_path: &Path) -> TestResult<Option<u32>> {
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if process_args_match_socket(pid, socket_path) {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn child_pids(parent_pid: u32) -> io::Result<Vec<u32>> {
    let children = fs::read_to_string(format!("/proc/{parent_pid}/task/{parent_pid}/children"))?;
    Ok(children
        .split_whitespace()
        .filter_map(|pid| pid.parse::<u32>().ok())
        .collect())
}

#[cfg(target_os = "linux")]
fn process_args_match_socket(pid: u32, socket_path: &Path) -> bool {
    let socket_bytes = socket_path.as_os_str().as_bytes();
    let Ok(args) = proc_args(pid) else {
        return false;
    };
    let has_socket_flag = args.iter().any(|arg| arg.as_slice() == b"--socket");
    let has_socket_path = args.iter().any(|arg| arg.as_slice() == socket_bytes);
    has_socket_flag && has_socket_path
}

#[cfg(target_os = "macos")]
fn find_daemon_pid(socket_path: &Path, _proxy_pid: Option<u32>) -> TestResult<Option<u32>> {
    let socket = socket_path.to_string_lossy();
    let output = Command::new("ps")
        .args(["-axww", "-o", "pid=", "-o", "command="])
        .output()?;
    if !output.status.success() {
        return Err(other(format!(
            "ps failed while locating daemon: status {} stderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim_start();
        let Some((pid, command)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        if command.contains("--socket") && command.contains(socket.as_ref()) {
            if let Ok(pid) = pid.parse::<u32>() {
                return Ok(Some(pid));
            }
        }
    }

    Ok(None)
}

#[cfg(target_os = "linux")]
fn proc_args(pid: u32) -> io::Result<Vec<Vec<u8>>> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline"))?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

#[cfg(target_os = "linux")]
fn process_command_line(pid: u32) -> TestResult<String> {
    let args = proc_args(pid)?;
    Ok(args
        .iter()
        .map(|arg| String::from_utf8_lossy(arg))
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(target_os = "macos")]
fn process_command_line(pid: u32) -> TestResult<String> {
    let output = Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(other(format!(
            "ps failed for pid {pid}: status {} stderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn terminate_pid(pid: u32) {
    let pid = pid.to_string();
    let _ = Command::new("kill")
        .args(["-TERM", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(100));
    if pid_exists(&pid) {
        let _ = Command::new("kill")
            .args(["-KILL", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn pid_exists(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_lossy(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) if bytes.is_empty() => "<empty>".to_string(),
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => "<missing>".to_string(),
        Err(error) => format!("<failed to read {}: {error}>", path.display()),
    }
}

fn other(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::Other, message.into()))
}
