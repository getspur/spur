// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{env, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use jute::state::State;
use spur_core::notebook::notebook_binary_path;
use spur_notebook::mcp::{self, bridge::AgentBridge};
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn handle_file_associations(
    app: &AppHandle,
    files: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    for file in files {
        jute::window::open_notebook_path(app, file)?;
    }
    Ok(())
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
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mcp-proxy" => {
                let socket_path = args
                    .next()
                    .map(PathBuf::from)
                    .expect("--mcp-proxy requires <path>");
                return Mode::McpProxy { socket_path };
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
    match UnixStream::connect(socket_path).await {
        Ok(stream) => Ok(stream),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            spawn_notebook_app(socket_path)?;
            wait_for_daemon_socket(socket_path).await
        }
        Err(error) => Err(error).map_err(Into::into),
    }
}

fn spawn_notebook_app(socket_path: &PathBuf) -> anyhow::Result<()> {
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
        .arg("--socket")
        .arg(socket_path)
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

async fn wait_for_daemon_socket(socket_path: &PathBuf) -> anyhow::Result<UnixStream> {
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

    #[allow(unused_mut)]
    let mut app = tauri::Builder::default();

    #[cfg(target_os = "macos")]
    {
        app = app.plugin(jute::plugins::macos_traffic_lights::init());
    }

    app.manage(State::new())
        .manage(bridge_for_state)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            jute::commands::kernel_usage_info,
            jute::commands::kernel_slot_info,
            jute::commands::start_kernel,
            jute::commands::restart_kernel,
            jute::commands::stop_kernel,
            jute::commands::run_cell,
            jute::commands::interrupt_kernel,
            jute::commands::get_notebook,
            jute::commands::save_to_disk,
            jute::commands::venv::venv_list_python_versions,
            jute::commands::venv::venv_create,
            jute::commands::venv::venv_list,
            jute::commands::venv::venv_delete,
            spur_notebook::mcp::bridge::bridge_ready,
            spur_notebook::mcp::bridge::notebook_active_changed,
            spur_notebook::mcp::bridge::agent_response,
        ])
        .setup(move |app| {
            let server_bridge = Arc::clone(&bridge_for_setup);
            let server_socket_path = socket_path.clone();
            let server_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let result =
                    mcp::start_daemon_server(server_socket_path, server_bridge, server_app)
                        .await
                        .map(|(handle, _control)| handle);

                match result {
                    Ok(_handle) => std::future::pending::<()>().await,
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
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(
            #[allow(unused_variables)]
            move |app, event| match event {
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
}
