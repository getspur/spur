// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env,
    ffi::OsString,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::Context as _;
use directories::BaseDirs;
use jute::state::State;
use spur_core::notebook::notebook_binary_path;
use spur_notebook::mcp::{self, bridge::AgentBridge};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const DAEMON_SPAWN_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_SPAWN_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

fn handle_file_associations(
    app: &AppHandle,
    files: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let targets = resolve_file_association_targets(files)?;
    for file in &targets {
        jute::window::open_notebook_path(app, file)?;
    }
    Ok(())
}

fn resolve_file_association_targets(files: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    if !files.iter().any(|file| is_spur_app_path(file)) {
        return Ok(files.to_vec());
    }

    let cache_root = default_spur_app_import_cache_root()?;
    resolve_file_association_targets_with_cache_root(files, &cache_root)
}

fn resolve_file_association_targets_with_cache_root(
    files: &[PathBuf],
    cache_root: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    files
        .iter()
        .map(|file| {
            if is_spur_app_path(file) {
                let imported = spur_notebook::spur_app::import_spur_app(file, cache_root)
                    .with_context(|| format!("failed to import {}", file.display()))?;
                Ok(imported.notebook_path)
            } else {
                Ok(file.clone())
            }
        })
        .collect()
}

fn is_spur_app_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(extension)
            if extension.eq_ignore_ascii_case(spur_notebook::spur_app::SPUR_APP_EXTENSION)
    )
}

fn default_spur_app_import_cache_root() -> anyhow::Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join("spurapps")
        .join("cache"))
}

enum Mode {
    App {
        files: Vec<PathBuf>,
        socket: Option<PathBuf>,
    },
    McpProxy {
        socket_path: PathBuf,
    },
}

fn parse_mode_from(args: impl IntoIterator<Item = String>) -> Mode {
    let mut files = Vec::new();
    let mut socket = None;
    let mut mcp_proxy_socket = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mcp-proxy" => {
                let socket_path = args
                    .next()
                    .map(PathBuf::from)
                    .expect("--mcp-proxy requires <path>");
                mcp_proxy_socket = Some(socket_path);
            }
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            _ if arg.starts_with('-') => {}
            _ => {
                if let Ok(url) = url::Url::parse(&arg) {
                    if url.scheme() == "file" {
                        if let Ok(path) = url.to_file_path() {
                            files.push(path);
                        }
                    }
                } else {
                    files.push(PathBuf::from(arg));
                }
            }
        }
    }
    if let Some(socket_path) = mcp_proxy_socket {
        return Mode::McpProxy { socket_path };
    }
    Mode::App { files, socket }
}

fn parse_mode() -> Mode {
    parse_mode_from(env::args().skip(1))
}

async fn run_mcp_proxy(socket_path: PathBuf) -> anyhow::Result<()> {
    let stream = connect_or_spawn_daemon(&socket_path).await?;
    let (mut socket_reader, mut socket_writer) = stream.into_split();

    let stdin_to_socket = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            spur_notebook::mcp::transport::write_frame_bytes(&mut socket_writer, line.as_bytes())
                .await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    let socket_to_stdout = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        loop {
            let bytes = spur_notebook::mcp::transport::read_frame_bytes(&mut socket_reader).await?;
            stdout.write_all(&bytes).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    tokio::select! {
        result = stdin_to_socket => result??,
        result = socket_to_stdout => result??,
    }
    Ok(())
}

async fn connect_or_spawn_daemon(socket_path: &PathBuf) -> anyhow::Result<UnixStream> {
    connect_or_spawn_daemon_with(socket_path, &spawn_notebook_app).await
}

async fn connect_or_spawn_daemon_with<F>(
    socket_path: &Path,
    spawn_daemon: &F,
) -> anyhow::Result<UnixStream>
where
    F: Fn(&Path) -> anyhow::Result<()> + Sync,
{
    if let Some(stream) = try_connect_daemon(socket_path).await? {
        return Ok(stream);
    }

    let _spawn_lock = acquire_daemon_spawn_lock(socket_path).await?;
    if let Some(stream) = try_connect_daemon(socket_path).await? {
        return Ok(stream);
    }

    spawn_daemon(socket_path)?;
    wait_for_daemon_socket(socket_path).await
}

async fn try_connect_daemon(socket_path: &Path) -> anyhow::Result<Option<UnixStream>> {
    match UnixStream::connect(socket_path).await {
        Ok(stream) => Ok(Some(stream)),
        Err(error) if daemon_socket_needs_spawn(error.kind()) => Ok(None),
        Err(error) => Err(error).map_err(Into::into),
    }
}

fn daemon_socket_needs_spawn(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::NotFound | ErrorKind::ConnectionRefused)
}

struct DaemonSpawnLock {
    _file: std::fs::File,
}

async fn acquire_daemon_spawn_lock(socket_path: &Path) -> anyhow::Result<DaemonSpawnLock> {
    use fs4::fs_std::FileExt as _;

    let lock_path = daemon_spawn_lock_path(socket_path);
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    let deadline = tokio::time::Instant::now() + DAEMON_SPAWN_LOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(true) => return Ok(DaemonSpawnLock { _file: file }),
            Ok(false) => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to acquire notebook daemon spawn lock {}",
                        lock_path.display()
                    )
                });
            }
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "timed out acquiring notebook daemon spawn lock {}",
                lock_path.display()
            );
        }
        tokio::time::sleep(DAEMON_SPAWN_LOCK_RETRY_INTERVAL.min(deadline - now)).await;
    }
}

fn daemon_spawn_lock_path(socket_path: &Path) -> PathBuf {
    let mut lock_path = socket_path.as_os_str().to_owned();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn spawn_notebook_app(socket_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_path = socket_path
        .parent()
        .map(|parent| parent.join("daemon.log"))
        .unwrap_or_else(|| PathBuf::from("spur-notebook-daemon.log"));
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());

    let program = std::env::current_exe().unwrap_or_else(|_| notebook_binary_path());
    let mut command = std::process::Command::new(program);
    command
        .args(notebook_daemon_args(socket_path))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()?;
    Ok(())
}

fn notebook_daemon_args(socket_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--socket"),
        socket_path.as_os_str().to_owned(),
    ]
}

async fn wait_for_daemon_socket(socket_path: &Path) -> anyhow::Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let error = match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error) => error,
        };
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "notebook daemon did not become ready at {} within 30s; last error: {}",
                socket_path.display(),
                error
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn main() {
    tracing_subscriber::fmt().init();

    let (files, socket_path) = match parse_mode() {
        Mode::App {
            files,
            socket: Some(socket_path),
        } => (files, socket_path),
        Mode::App { socket: None, .. } => {
            eprintln!("spur-notebook app mode requires --socket <path>");
            std::process::exit(2);
        }
        Mode::McpProxy { socket_path } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            runtime
                .block_on(run_mcp_proxy(socket_path))
                .expect("notebook MCP proxy failed");
            return;
        }
    };

    let bridge = Arc::new(AgentBridge::new());
    let bridge_for_state = Arc::clone(&bridge);
    let bridge_for_setup = Arc::clone(&bridge);
    let bridge_for_run = Arc::clone(&bridge);
    let state = Arc::new(State::new());
    let state_for_manage = Arc::clone(&state);
    let state_for_setup = Arc::clone(&state);
    let daemon_control: spur_notebook::commands::NotebookDaemonControlSlot =
        Arc::new(tokio::sync::Mutex::new(None));
    let daemon_control_for_manage = Arc::clone(&daemon_control);
    let daemon_control_for_setup = Arc::clone(&daemon_control);
    #[cfg(target_os = "macos")]
    let daemon_control_for_run = Arc::clone(&daemon_control);
    #[expect(
        clippy::exit,
        reason = "tauri::generate_context! expands through process::exit"
    )]
    let context = tauri::generate_context!();

    tauri::Builder::default()
        .manage(state_for_manage)
        .manage(daemon_control_for_manage)
        .manage(bridge_for_state)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            jute::commands::kernel_usage_info,
            jute::commands::kernel_slot_info,
            jute::commands::move_notebook_to_trash,
            jute::commands::reveal_notebook_in_finder,
            jute::commands::daemon_control,
            jute::commands::discard_scratch_notebooks,
            jute::commands::start_kernel,
            jute::commands::restart_kernel,
            jute::commands::stop_kernel,
            jute::commands::run_cell,
            jute::commands::interrupt_kernel,
            jute::commands::get_notebook,
            jute::commands::save_to_disk,
            jute::commands::spur_delegate_to_worker,
            jute::commands::venv::venv_list_python_versions,
            jute::commands::venv::venv_create,
            jute::commands::venv::venv_list,
            jute::commands::venv::venv_delete,
            spur_notebook::commands::notebook_dag_status,
            spur_notebook::commands::notebook_run_cascade,
            spur_notebook::commands::notebook_run_cell,
            spur_notebook::commands::anywidget_command,
            spur_notebook::commands::publish_spur_app,
            spur_notebook::mcp::bridge::bridge_ready,
            spur_notebook::mcp::bridge::notebook_active_changed,
            spur_notebook::mcp::bridge::agent_response,
        ])
        .setup(move |app| {
            let server_bridge = Arc::clone(&bridge_for_setup);
            let server_socket_path = socket_path.clone();
            let server_app = app.handle().clone();
            let server_state = Arc::clone(&state_for_setup);
            jute::spawn_notebook_delta_forwarder(
                app.handle().clone(),
                Arc::clone(&state_for_setup),
            );
            jute::spawn_datasources_changed_forwarder(
                app.handle().clone(),
                Arc::clone(&state_for_setup),
            );
            match app.path().resource_dir() {
                Ok(resource_root) => {
                    match spur_notebook::extension_install::install_bundled_extension(
                        &resource_root,
                    ) {
                        Ok(Some(dest)) => tracing::info!(
                            dest = %dest.display(),
                            "installed bundled spur_rest duckdb extension"
                        ),
                        Ok(None) => tracing::debug!(
                            "spur_rest extension already present or not bundled; skipping install"
                        ),
                        Err(error) => tracing::warn!(
                            %error,
                            "failed to install bundled spur_rest duckdb extension"
                        ),
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    "could not resolve tauri resource dir for extension install"
                ),
            }
            let daemon_control = Arc::clone(&daemon_control_for_setup);
            tauri::async_runtime::spawn(async move {
                match mcp::start_daemon_server(
                    server_socket_path,
                    server_bridge,
                    server_app,
                    server_state,
                )
                .await
                {
                    Ok((handle, control)) => {
                        let mut slot = daemon_control.lock().await;
                        *slot = Some(Arc::new(control));
                        drop(slot);

                        let keep_alive = handle;
                        std::future::pending::<()>().await;
                        drop(keep_alive);
                    }
                    Err(error) => tracing::error!(%error, "failed to start notebook MCP server"),
                }
            });

            // Linux/Windows: only honor explicit file args. The no-file case is
            // handled by `restore_last_open_notebook` (called from start_daemon_server)
            // which opens last.json or creates a fresh Untitled — opening a "home"
            // window here would race the restore (see RunEvent::Ready comment below).
            if cfg!(any(windows, target_os = "linux")) && !files.is_empty() {
                handle_file_associations(app.handle(), &files)?;
            }

            Ok(())
        })
        .menu(jute::menu::setup_menu)
        .build(context)
        .expect("error while running tauri application")
        .run(
            #[allow(unused_variables)]
            move |app, event| match event {
                #[cfg(target_os = "macos")]
                tauri::RunEvent::ExitRequested { api, .. } => {
                    api.prevent_exit();
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Exit => {
                    let bridge = Arc::clone(&bridge_for_run);
                    tauri::async_runtime::spawn(async move {
                        bridge.drain_on_shutdown().await;
                    });
                }
                #[cfg(not(target_os = "macos"))]
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => {
                    let bridge = Arc::clone(&bridge_for_run);
                    tauri::async_runtime::spawn(async move {
                        bridge.drain_on_shutdown().await;
                    });
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Opened { urls } => {
                    let files = urls
                        .into_iter()
                        .filter_map(|url| url.to_file_path().ok())
                        .collect::<Vec<_>>();
                    handle_file_associations(app, &files).unwrap();
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen {
                    has_visible_windows,
                    ..
                } => {
                    if !has_visible_windows {
                        let daemon_control = Arc::clone(&daemon_control_for_run);
                        tauri::async_runtime::spawn(async move {
                            let control = {
                                let slot = daemon_control.lock().await;
                                slot.as_ref().map(Arc::clone)
                            };

                            let Some(control) = control else {
                                tracing::warn!(
                                    "cannot reopen notebook window before daemon control is ready"
                                );
                                return;
                            };

                            if let Err(error) = control.reopen().await {
                                tracing::warn!(%error, "failed to reopen notebook window");
                            }
                        });
                    }
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Ready => {
                    // Do NOT open the home page here. `restore_last_open_notebook`
                    // (spawned from `setup`) opens either the last notebook or a
                    // fresh Untitled. Opening a second "home" window races the
                    // restore: both webviews mount React, both run setActiveAgentNotebook,
                    // and whichever runs last wins — the home page's
                    // `setActiveAgentNotebook(undefined)` would clobber the
                    // notebook page's set-to-active, leaving the bridge
                    // permanently `notebook_open = false`.
                }
                _ => {}
            },
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_proxy_requires_explicit_socket_path() {
        let Mode::McpProxy { socket_path } = parse_mode_from([
            "--mcp-proxy".to_string(),
            "/tmp/notebook-session.sock".to_string(),
        ]) else {
            panic!("expected mcp proxy mode");
        };

        assert_eq!(socket_path, PathBuf::from("/tmp/notebook-session.sock"));
    }

    #[test]
    fn app_mode_accepts_explicit_socket_path() {
        let Mode::App { files, socket } = parse_mode_from([
            "--socket".to_string(),
            "/tmp/notebook-session.sock".to_string(),
            "something.ipynb".to_string(),
        ]) else {
            panic!("expected app mode");
        };

        assert_eq!(socket, Some(PathBuf::from("/tmp/notebook-session.sock")));
        assert_eq!(files, vec![PathBuf::from("something.ipynb")]);
    }

    #[test]
    fn app_mode_collects_file_args() {
        let Mode::App { files, socket } = parse_mode_from(["something.ipynb".to_string()]) else {
            panic!("expected app mode");
        };

        assert_eq!(socket, None);
        assert_eq!(files, vec![PathBuf::from("something.ipynb")]);
    }

    #[test]
    fn file_association_app_mode_collects_spurapp_file_args() {
        let Mode::App { files, socket } = parse_mode_from([
            "--socket".to_string(),
            "/tmp/notebook-session.sock".to_string(),
            "forecast.spurapp".to_string(),
            "notes.ipynb".to_string(),
        ]) else {
            panic!("expected app mode");
        };

        assert_eq!(socket, Some(PathBuf::from("/tmp/notebook-session.sock")));
        assert_eq!(
            files,
            vec![
                PathBuf::from("forecast.spurapp"),
                PathBuf::from("notes.ipynb")
            ]
        );
    }

    #[test]
    fn file_association_resolve_keeps_ipynb_and_imports_spurapp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_notebook = temp.path().join("source.ipynb");
        let package = temp.path().join("forecast.spurapp");
        let cache_root = temp.path().join("cache");
        let notes = temp.path().join("notes.ipynb");

        std::fs::write(
            &source_notebook,
            r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
        )
        .expect("seed source notebook");
        std::fs::write(
            &notes,
            r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
        )
        .expect("seed notes notebook");

        spur_notebook::spur_app::export_spur_app(spur_notebook::spur_app::SpurAppExportOptions {
            notebook_path: source_notebook.clone(),
            output_path: package.clone(),
            name: Some("Forecast Dashboard".to_string()),
            widget_assets: Vec::new(),
            include_port_snapshots: false,
            dependency_roots: vec![temp.path().to_path_buf()],
        })
        .expect("export spurapp");

        let resolved = resolve_file_association_targets_with_cache_root(
            &[notes.clone(), package.clone()],
            &cache_root,
        )
        .expect("resolve file associations");

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], notes);
        assert_eq!(
            resolved[1].file_name().and_then(|name| name.to_str()),
            Some(spur_notebook::spur_app::SPUR_APP_ENTRY_NOTEBOOK)
        );
        assert!(resolved[1].starts_with(&cache_root));
        assert_eq!(
            std::fs::read_to_string(&resolved[1]).expect("read imported notebook"),
            std::fs::read_to_string(&source_notebook).expect("read source notebook")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_daemon_connects_share_one_spawned_listener() {
        use std::io::Read;
        use std::os::unix::net::UnixListener as StdUnixListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread::JoinHandle;
        use tokio::io::AsyncWriteExt as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("shared.sock");
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let listener_thread = Arc::new(Mutex::new(None::<JoinHandle<std::io::Result<()>>>));

        let spawn_count_for_spawn = Arc::clone(&spawn_count);
        let listener_thread_for_spawn = Arc::clone(&listener_thread);
        let spawn_daemon = move |socket_path: &Path| -> anyhow::Result<()> {
            if spawn_count_for_spawn.fetch_add(1, Ordering::SeqCst) != 0 {
                anyhow::bail!("spawn hook called more than once");
            }
            std::thread::sleep(Duration::from_millis(100));
            if let Some(parent) = socket_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let listener = StdUnixListener::bind(socket_path)?;
            let handle = std::thread::spawn(move || -> std::io::Result<()> {
                let (mut first, _) = listener.accept()?;
                let (mut second, _) = listener.accept()?;
                let mut byte = [0_u8; 1];
                first.read_exact(&mut byte)?;
                second.read_exact(&mut byte)?;
                Ok(())
            });
            *listener_thread_for_spawn
                .lock()
                .expect("listener thread lock") = Some(handle);
            Ok(())
        };

        let (first, second) = tokio::join!(
            connect_or_spawn_daemon_with(&socket_path, &spawn_daemon),
            connect_or_spawn_daemon_with(&socket_path, &spawn_daemon)
        );
        let mut first = first.expect("first caller should connect");
        let mut second = second.expect("second caller should connect");

        first.write_all(b"x").await.expect("write first byte");
        second.write_all(b"y").await.expect("write second byte");
        drop(first);
        drop(second);

        let handle = listener_thread
            .lock()
            .expect("listener thread lock")
            .take()
            .expect("listener thread should be spawned");
        handle
            .join()
            .expect("listener thread panicked")
            .expect("listener");
        let lock = tokio::time::timeout(
            Duration::from_secs(1),
            acquire_daemon_spawn_lock(&socket_path),
        )
        .await
        .expect("spawn lock should not leak")
        .expect("reacquire spawn lock");
        drop(lock);
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
    }
}
