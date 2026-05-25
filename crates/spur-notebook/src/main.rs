// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use jute::state::State;
use spur_core::notebook::notebook_binary_path;
#[cfg(target_os = "macos")]
use spur_notebook::mcp::NotebookDaemonControl;
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
        config: mcp::NotebookConfig,
    },
    McpProxy {
        socket_path: PathBuf,
        config: mcp::NotebookConfig,
    },
}

fn parse_mode_from(args: impl IntoIterator<Item = String>) -> Mode {
    parse_mode_from_with_config(args, notebook_config_from_env())
}

fn parse_mode_from_with_config(
    args: impl IntoIterator<Item = String>,
    mut config: mcp::NotebookConfig,
) -> Mode {
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
            "--notebook-in-proc-store" => {
                config.in_proc_store = true;
            }
            "--no-notebook-in-proc-store" => {
                config.in_proc_store = false;
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
        return Mode::McpProxy {
            socket_path,
            config,
        };
    }
    Mode::App {
        files,
        socket,
        config,
    }
}

fn parse_mode() -> Mode {
    parse_mode_from(env::args().skip(1))
}

fn notebook_config_from_env() -> mcp::NotebookConfig {
    mcp::NotebookConfig {
        // Defaults to true (in-proc store on); jute::notebook_in_proc_store_enabled
        // honors SPUR_NOTEBOOK_IN_PROC_STORE=0/false/no/off as the opt-out.
        in_proc_store: jute::notebook_in_proc_store_enabled(),
    }
}

async fn run_mcp_proxy(socket_path: PathBuf, config: mcp::NotebookConfig) -> anyhow::Result<()> {
    let stream = connect_or_spawn_daemon(&socket_path, config).await?;
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

async fn connect_or_spawn_daemon(
    socket_path: &PathBuf,
    config: mcp::NotebookConfig,
) -> anyhow::Result<UnixStream> {
    match UnixStream::connect(socket_path).await {
        Ok(stream) => Ok(stream),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            spawn_notebook_app(socket_path, config)?;
            wait_for_daemon_socket(socket_path).await
        }
        Err(error) => Err(error).map_err(Into::into),
    }
}

fn spawn_notebook_app(socket_path: &PathBuf, config: mcp::NotebookConfig) -> anyhow::Result<()> {
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
        .args(notebook_daemon_args(socket_path, config))
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

fn notebook_daemon_args(socket_path: &Path, config: mcp::NotebookConfig) -> Vec<OsString> {
    vec![
        OsString::from("--socket"),
        socket_path.as_os_str().to_owned(),
        OsString::from(if config.in_proc_store {
            "--notebook-in-proc-store"
        } else {
            "--no-notebook-in-proc-store"
        }),
    ]
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

    let (files, socket_path, config) = match parse_mode() {
        Mode::App {
            files,
            socket: Some(socket_path),
            config,
        } => (files, socket_path, config),
        Mode::App { socket: None, .. } => {
            eprintln!("spur-notebook app mode requires --socket <path>");
            std::process::exit(2);
        }
        Mode::McpProxy {
            socket_path,
            config,
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            runtime
                .block_on(run_mcp_proxy(socket_path, config))
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
    #[cfg(target_os = "macos")]
    let daemon_control = Arc::new(tokio::sync::Mutex::new(None::<Arc<NotebookDaemonControl>>));
    #[cfg(target_os = "macos")]
    let daemon_control_for_setup = Arc::clone(&daemon_control);
    #[cfg(target_os = "macos")]
    let daemon_control_for_run = Arc::clone(&daemon_control);

    tauri::Builder::default()
        .manage(state_for_manage)
        .manage(bridge_for_state)
        .manage(jute::commands::NotebookRuntimeConfig {
            in_proc_store: config.in_proc_store,
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            jute::commands::kernel_usage_info,
            jute::commands::kernel_slot_info,
            jute::commands::list_recent_notebooks,
            jute::commands::remove_notebook_from_recents,
            jute::commands::set_notebook_pinned,
            jute::commands::move_notebook_to_trash,
            jute::commands::reveal_notebook_in_finder,
            jute::commands::new_notebook_via_daemon,
            jute::commands::reopen_notebook_via_daemon,
            jute::commands::close_notebook_via_daemon,
            jute::commands::discard_scratch_notebooks,
            jute::commands::start_kernel,
            jute::commands::restart_kernel,
            jute::commands::stop_kernel,
            jute::commands::run_cell,
            jute::commands::interrupt_kernel,
            jute::commands::get_notebook,
            jute::commands::read_notebook_store_cell,
            jute::commands::notebook_store_snapshot,
            jute::commands::save_to_disk,
            jute::commands::venv::venv_list_python_versions,
            jute::commands::venv::venv_create,
            jute::commands::venv::venv_list,
            jute::commands::venv::venv_delete,
            jute::commands::notebook_runtime_config,
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
                config.in_proc_store,
            );
            #[cfg(target_os = "macos")]
            let daemon_control = Arc::clone(&daemon_control_for_setup);
            tauri::async_runtime::spawn(async move {
                match mcp::start_daemon_server_with_config(
                    server_socket_path,
                    server_bridge,
                    server_app,
                    server_state,
                    config,
                )
                .await
                {
                    Ok((handle, control)) => {
                        #[cfg(target_os = "macos")]
                        {
                            let mut slot = daemon_control.lock().await;
                            *slot = Some(Arc::new(control));
                        }
                        #[cfg(not(target_os = "macos"))]
                        let _control = control;

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
        .build(tauri::generate_context!())
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

    fn default_off_config() -> mcp::NotebookConfig {
        mcp::NotebookConfig {
            in_proc_store: false,
        }
    }

    #[test]
    fn mcp_proxy_requires_explicit_socket_path() {
        let Mode::McpProxy {
            socket_path,
            config,
        } = parse_mode_from_with_config(
            [
                "--mcp-proxy".to_string(),
                "/tmp/notebook-session.sock".to_string(),
            ],
            default_off_config(),
        )
        else {
            panic!("expected mcp proxy mode");
        };

        assert_eq!(socket_path, PathBuf::from("/tmp/notebook-session.sock"));
        assert!(!config.in_proc_store);
    }

    #[test]
    fn app_mode_accepts_explicit_socket_path() {
        let Mode::App {
            files,
            socket,
            config,
        } = parse_mode_from_with_config(
            [
                "--socket".to_string(),
                "/tmp/notebook-session.sock".to_string(),
                "something.ipynb".to_string(),
            ],
            default_off_config(),
        )
        else {
            panic!("expected app mode");
        };

        assert_eq!(socket, Some(PathBuf::from("/tmp/notebook-session.sock")));
        assert_eq!(files, vec![PathBuf::from("something.ipynb")]);
        assert!(!config.in_proc_store);
    }

    #[test]
    fn app_mode_collects_file_args() {
        let Mode::App {
            files,
            socket,
            config,
        } = parse_mode_from_with_config(["something.ipynb".to_string()], default_off_config())
        else {
            panic!("expected app mode");
        };

        assert_eq!(socket, None);
        assert_eq!(files, vec![PathBuf::from("something.ipynb")]);
        assert!(!config.in_proc_store);
    }

    #[test]
    fn app_mode_accepts_in_proc_store_flag() {
        let Mode::App { config, .. } = parse_mode_from_with_config(
            [
                "--notebook-in-proc-store".to_string(),
                "--socket".to_string(),
                "/tmp/notebook-session.sock".to_string(),
            ],
            default_off_config(),
        ) else {
            panic!("expected app mode");
        };

        assert!(config.in_proc_store);
    }

    #[test]
    fn mcp_proxy_accepts_in_proc_store_flag_after_socket_path() {
        let Mode::McpProxy { config, .. } = parse_mode_from_with_config(
            [
                "--mcp-proxy".to_string(),
                "/tmp/notebook-session.sock".to_string(),
                "--notebook-in-proc-store".to_string(),
            ],
            default_off_config(),
        ) else {
            panic!("expected mcp proxy mode");
        };

        assert!(config.in_proc_store);
    }

    #[test]
    fn app_mode_accepts_in_proc_store_env_fallback() {
        let Mode::App { config, .. } = parse_mode_from_with_config(
            [
                "--socket".to_string(),
                "/tmp/notebook-session.sock".to_string(),
            ],
            mcp::NotebookConfig {
                in_proc_store: true,
            },
        ) else {
            panic!("expected app mode");
        };

        assert!(config.in_proc_store);
    }

    #[test]
    fn cli_no_flag_disables_in_proc_store_default() {
        // Initial config is on (matches the new env default); --no-notebook-in-proc-store
        // explicitly opts out.
        let Mode::App { config, .. } = parse_mode_from_with_config(
            [
                "--no-notebook-in-proc-store".to_string(),
                "--socket".to_string(),
                "/tmp/notebook-session.sock".to_string(),
            ],
            mcp::NotebookConfig {
                in_proc_store: true,
            },
        ) else {
            panic!("expected app mode");
        };

        assert!(!config.in_proc_store);
    }

    #[test]
    fn daemon_spawn_args_round_trip_no_in_proc_store_opt_out() {
        let socket_path = PathBuf::from("/tmp/notebook-session.sock");
        let args = notebook_daemon_args(
            &socket_path,
            mcp::NotebookConfig {
                in_proc_store: false,
            },
        )
        .into_iter()
        .map(|arg| arg.into_string().expect("test args should be utf-8"))
        .collect::<Vec<_>>();

        let Mode::App { socket, config, .. } = parse_mode_from_with_config(
            args,
            mcp::NotebookConfig {
                in_proc_store: true,
            },
        ) else {
            panic!("expected app mode");
        };

        assert_eq!(socket, Some(socket_path));
        assert!(!config.in_proc_store);
    }
}
