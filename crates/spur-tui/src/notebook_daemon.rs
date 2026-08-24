use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{io, process::Stdio, time::Duration};

use serde::Deserialize;
#[cfg(unix)]
use serde::Serialize;
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::sleep,
};

#[cfg(unix)]
const FRAME_LIMIT: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const CONNECT_ATTEMPTS: usize = 5;
#[cfg(unix)]
const CONNECT_INITIAL_DELAY: Duration = Duration::from_millis(100);
#[cfg(unix)]
const CONNECT_MAX_DELAY: Duration = Duration::from_millis(800);
#[cfg(unix)]
const LAUNCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const LAUNCH_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(250);

#[cfg(unix)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlRequest<'a> {
    daemon: &'static str,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_root: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    pub ok: bool,
    pub path: Option<String>,
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

pub async fn send_notebook_command(
    arg: &str,
    socket_path: &Path,
    project_root: &Path,
) -> anyhow::Result<ControlResponse> {
    match parse_notebook_command(arg)? {
        NotebookCommand::Reopen => {
            send_control("reopen", None, None, None, socket_path, project_root).await
        }
        NotebookCommand::New => {
            send_control("new", None, None, None, socket_path, project_root).await
        }
        NotebookCommand::Close => {
            send_control("close", None, None, None, socket_path, project_root).await
        }
        NotebookCommand::Open { path } => {
            send_control("open", Some(&path), None, None, socket_path, project_root).await
        }
        NotebookCommand::AttachDatasource { path, name, group } => {
            send_control(
                "attach_datasource",
                Some(&path),
                Some(&name),
                group.as_deref(),
                socket_path,
                project_root,
            )
            .await
        }
    }
}

pub(crate) fn project_root_from_config_path(config_path: Option<&Path>) -> PathBuf {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    project_root_from_config_path_and_cwd(config_path, &current_dir)
}

fn project_root_from_config_path_and_cwd(config_path: Option<&Path>, cwd: &Path) -> PathBuf {
    let candidate = config_path
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.to_path_buf());

    spur_core::project_root::discover(&candidate)
        .or_else(|_| candidate.canonicalize())
        .unwrap_or(candidate)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NotebookCommand {
    Reopen,
    New,
    Close,
    Open {
        path: String,
    },
    AttachDatasource {
        path: String,
        name: String,
        group: Option<String>,
    },
}

fn parse_notebook_command(arg: &str) -> anyhow::Result<NotebookCommand> {
    let trimmed = arg.trim();
    match trimmed {
        "" => return Ok(NotebookCommand::Reopen),
        "new" => return Ok(NotebookCommand::New),
        "close" => return Ok(NotebookCommand::Close),
        _ => {}
    }

    if let Some(rest) = strip_data_add(trimmed) {
        return parse_attach_datasource(rest);
    }

    Ok(NotebookCommand::Open {
        path: trimmed.to_string(),
    })
}

fn strip_data_add(trimmed: &str) -> Option<&str> {
    if trimmed == "data add" {
        Some("")
    } else {
        trimmed.strip_prefix("data add ").map(str::trim_start)
    }
}

fn parse_attach_datasource(rest: &str) -> anyhow::Result<NotebookCommand> {
    let tokens = split_notebook_args(rest)?;
    let Some(path) = tokens.first() else {
        anyhow::bail!("usage: /notebook data add <path> [--name X] [--group G]");
    };
    if path.starts_with("--") {
        anyhow::bail!("usage: /notebook data add <path> [--name X] [--group G]");
    }

    let mut name = None;
    let mut group = None;
    let mut index = 1;
    while index < tokens.len() {
        let token = &tokens[index];
        match token.as_str() {
            "--name" => {
                index += 1;
                let Some(value) = tokens.get(index) else {
                    anyhow::bail!("--name requires a value");
                };
                name = Some(value.clone());
            }
            "--group" => {
                index += 1;
                let Some(value) = tokens.get(index) else {
                    anyhow::bail!("--group requires a value");
                };
                group = Some(value.clone());
            }
            _ if token.starts_with("--name=") => {
                name = Some(token["--name=".len()..].to_string());
            }
            _ if token.starts_with("--group=") => {
                group = Some(token["--group=".len()..].to_string());
            }
            _ if token.starts_with("--") => {
                anyhow::bail!("unknown /notebook data add option: {token}");
            }
            _ => {
                anyhow::bail!("unexpected /notebook data add argument: {token}");
            }
        }
        index += 1;
    }

    let name = match name {
        Some(name) if !name.trim().is_empty() => name,
        Some(_) => anyhow::bail!("--name requires a non-empty value"),
        None => infer_datasource_name(path)?,
    };
    if group
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        anyhow::bail!("--group requires a non-empty value");
    }

    Ok(NotebookCommand::AttachDatasource {
        path: path.clone(),
        name,
        group,
    })
}

fn split_notebook_args(input: &str) -> anyhow::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaping = false;

    for ch in input.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }
        if ch == '\\' {
            escaping = true;
            continue;
        }
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    if quote.is_some() {
        anyhow::bail!("unterminated quote in /notebook data add");
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn infer_datasource_name(path: &str) -> anyhow::Result<String> {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("could not infer datasource name from path; pass --name"))
}

/// The notebook daemon control socket is a unix domain socket; on other
/// platforms every /notebook command fails with a clear platform error.
#[cfg(not(unix))]
async fn send_control(
    _command: &str,
    _path: Option<&str>,
    _name: Option<&str>,
    _group: Option<&str>,
    _socket_path: &Path,
    _project_root: &Path,
) -> anyhow::Result<ControlResponse> {
    anyhow::bail!("the notebook daemon requires unix domain sockets, unavailable on this platform")
}

#[cfg(unix)]
async fn send_control(
    command: &str,
    path: Option<&str>,
    name: Option<&str>,
    group: Option<&str>,
    socket_path: &Path,
    project_root: &Path,
) -> anyhow::Result<ControlResponse> {
    let mut stream = connect_or_launch_control_socket(socket_path, |socket_path| {
        launch_notebook_app(socket_path, project_root)
    })
    .await?;
    let request = ControlRequest {
        daemon: "notebook.v1",
        command,
        project_root: Some(project_root),
        path,
        name,
        group,
    };
    let bytes = serde_json::to_vec(&request)?;
    write_frame(&mut stream, &bytes).await?;
    let response = read_frame(&mut stream).await?;
    Ok(serde_json::from_slice(&response)?)
}

#[cfg(unix)]
async fn connect_or_launch_control_socket(
    socket_path: &Path,
    launch: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<UnixStream> {
    match connect_control_socket(socket_path).await {
        Ok(stream) => return Ok(stream),
        Err(error) if should_retry_connect_error(&error) => {}
        Err(error) => return Err(error),
    }

    launch(socket_path)?;
    wait_for_launched_control_socket(socket_path).await
}

#[cfg(unix)]
async fn connect_control_socket(socket_path: &Path) -> io::Result<UnixStream> {
    let mut delay = CONNECT_INITIAL_DELAY;
    for attempt in 0..CONNECT_ATTEMPTS {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if should_retry_connect_error(&error) && attempt + 1 < CONNECT_ATTEMPTS => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(CONNECT_MAX_DELAY);
            }
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::other("notebook daemon connect retry exhausted"))
}

#[cfg(unix)]
fn should_retry_connect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NotFound
            | io::ErrorKind::AddrNotAvailable
    )
}

pub fn notebook_launch_report(selection: &spur_core::notebook::NotebookLaunchSelection) -> String {
    format!(
        "channel={} path={} reason={}",
        selection.channel,
        selection.path.display(),
        selection.reason
    )
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "pre_exec is required to detach the notebook child from the TUI's controlling session"
)]
fn detach_command_session(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: the child callback only invokes the async-signal-safe `setsid`
    // syscall and reports its errno. No allocation or lock is used after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
fn launch_notebook_app(socket_path: &Path, project_root: &Path) -> io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_path = socket_path
        .parent()
        .map(|parent| parent.join("daemon.log"))
        .unwrap_or_else(|| std::path::PathBuf::from("spur-notebook-daemon.log"));
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());

    let selection =
        spur_core::notebook::notebook_launch_selection().map_err(notebook_resolver_io_error)?;
    tracing::info!(
        channel = %selection.channel,
        path = %selection.path.display(),
        reason = %selection.reason,
        "notebook daemon launch selection: {}",
        notebook_launch_report(&selection)
    );

    let mut command = std::process::Command::new(&selection.path);
    command
        .arg("--socket")
        .arg(socket_path)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    detach_command_session(&mut command);
    command.spawn().map(|_| ())
}

#[cfg(unix)]
fn notebook_resolver_io_error(error: spur_core::notebook::NotebookResolverError) -> io::Error {
    let kind = match &error {
        spur_core::notebook::NotebookResolverError::InvalidChannel { .. } => {
            io::ErrorKind::InvalidInput
        }
        spur_core::notebook::NotebookResolverError::GreenUnavailable { .. } => {
            io::ErrorKind::NotFound
        }
    };
    io::Error::new(kind, error)
}

#[cfg(unix)]
async fn wait_for_launched_control_socket(socket_path: &Path) -> io::Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + LAUNCH_CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if tokio::time::Instant::now() >= deadline || !should_retry_connect_error(&error) {
                    return Err(error);
                }
            }
        }
        sleep(LAUNCH_CONNECT_RETRY_DELAY).await;
    }
}

#[cfg(unix)]
async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() > FRAME_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "notebook daemon frame too large",
        ));
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

#[cfg(unix)]
async fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > FRAME_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "notebook daemon frame too large",
        ));
    }
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

#[cfg(all(test, unix))]
#[expect(
    unsafe_code,
    reason = "process-global environment mutation and Unix session inspection are serialized test operations"
)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    use serde_json::json;
    use tokio::net::UnixListener;

    use super::*;

    static NOTEBOOK_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: `NOTEBOOK_ENV_LOCK` serializes this process-global mutation.
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                // SAFETY: the guard remains protected by `NOTEBOOK_ENV_LOCK` until drop.
                Some(value) => unsafe { std::env::set_var(self.name, value) },
                // SAFETY: the guard remains protected by `NOTEBOOK_ENV_LOCK` until drop.
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

    fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut suffixed = path.as_os_str().to_os_string();
        suffixed.push(suffix);
        PathBuf::from(suffixed)
    }

    fn session_id(pid: libc::pid_t) -> libc::pid_t {
        // SAFETY: `getsid` only reads kernel process metadata for the supplied PID.
        let session_id = unsafe { libc::getsid(pid) };
        assert_ne!(
            session_id,
            -1,
            "getsid({pid}) failed: {}",
            io::Error::last_os_error()
        );
        session_id
    }

    #[test]
    fn launch_notebook_app_starts_child_in_new_session() {
        let _env_lock = NOTEBOOK_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("create project root");
        let fake_notebook = temp.path().join("fake-notebook");
        fs::write(
            &fake_notebook,
            "#!/bin/sh\npwd > \"${2}.cwd\"\nprintf '%s\\n' \"$$\" > \"${2}.pid\"\nwhile [ -e \"${2}.hold\" ]; do\n  sleep 0.05\ndone\n",
        )
        .expect("write fake notebook");
        let mut permissions = fs::metadata(&fake_notebook)
            .expect("fake notebook metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_notebook, permissions).expect("make fake notebook executable");

        let socket_path = temp.path().join("notebook.sock");
        let cwd_path = path_with_suffix(&socket_path, ".cwd");
        let pid_path = path_with_suffix(&socket_path, ".pid");
        let hold_path = path_with_suffix(&socket_path, ".hold");
        fs::write(&hold_path, b"").expect("create child hold file");
        let _bin_override = EnvVarGuard::set("SPUR_NOTEBOOK_BIN", &fake_notebook);

        launch_notebook_app(&socket_path, &project_root).expect("launch fake notebook");

        let deadline = Instant::now() + Duration::from_secs(5);
        let child_pid = loop {
            match fs::read_to_string(&pid_path) {
                Ok(pid) => {
                    break pid
                        .trim()
                        .parse::<libc::pid_t>()
                        .expect("numeric child pid")
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("read child pid {}: {error}", pid_path.display()),
            }
        };

        let parent_pid = libc::pid_t::try_from(std::process::id()).expect("parent pid fits pid_t");
        let parent_session = session_id(parent_pid);
        let child_session = session_id(child_pid);
        let child_cwd = fs::read_to_string(&cwd_path).expect("read child cwd");
        fs::remove_file(&hold_path).expect("release fake notebook child");

        assert_eq!(Path::new(child_cwd.trim()), project_root);
        assert_ne!(
            child_session, parent_session,
            "notebook child must not share the TUI process session"
        );
    }

    #[test]
    fn project_root_comes_from_project_local_config_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        let spur_dir = project_root.join(".spur");
        fs::create_dir_all(&spur_dir).expect("create .spur directory");
        let config_path = spur_dir.join("config.toml");
        fs::write(&config_path, b"").expect("write config");

        assert_eq!(
            project_root_from_config_path(Some(&config_path)),
            project_root.canonicalize().expect("canonical project root")
        );
    }

    #[test]
    fn project_root_is_discovered_from_nested_working_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        let nested_dir = project_root.join("src").join("nested");
        fs::create_dir_all(project_root.join(".spur")).expect("create .spur directory");
        fs::create_dir_all(&nested_dir).expect("create nested working directory");

        assert_eq!(
            project_root_from_config_path_and_cwd(None, &nested_dir),
            project_root.canonicalize().expect("canonical project root")
        );
    }

    #[tokio::test]
    async fn connect_or_launch_control_socket_launches_when_socket_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("notebook.sock");
        let project_root = temp.path().join("project");
        let expected_project_root = project_root.clone();
        let launched = Arc::new(AtomicBool::new(false));
        let launched_for_fn = Arc::clone(&launched);
        let socket_for_server = socket_path.clone();

        let mut stream = connect_or_launch_control_socket(&socket_path, move |path| {
            assert_eq!(path, socket_for_server.as_path());
            launched_for_fn.store(true, Ordering::SeqCst);
            let listener = UnixListener::bind(&socket_for_server)?;
            let expected_project_root = expected_project_root.clone();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept control socket");
                let request = read_frame(&mut stream).await.expect("read request frame");
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&request).expect("json"),
                    json!({
                        "daemon":"notebook.v1",
                        "command":"reopen",
                        "projectRoot": expected_project_root,
                    })
                );
                write_frame(&mut stream, br#"{"ok":true,"path":"/tmp/notebook.ipynb"}"#)
                    .await
                    .expect("write response");
            });
            Ok(())
        })
        .await
        .expect("connect after launch");

        let request = serde_json::to_vec(&ControlRequest {
            daemon: "notebook.v1",
            command: "reopen",
            project_root: Some(&project_root),
            path: None,
            name: None,
            group: None,
        })
        .expect("serialize request");
        write_frame(&mut stream, &request)
            .await
            .expect("write request");
        let response = read_frame(&mut stream).await.expect("read response");

        assert!(launched.load(Ordering::SeqCst));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response).expect("json response"),
            json!({"ok":true,"path":"/tmp/notebook.ipynb"})
        );
    }
}
