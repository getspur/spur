// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{env, path::PathBuf, sync::Arc};

use jute::state::State;
use spur_core::notebook::control_socket_path;
use spur_notebook::mcp::{self, bridge::AgentBridge};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn handle_file_associations(
    app: &AppHandle,
    files: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    for file in files {
        jute::window::open_notebook_path(app, file)?;
    }
    Ok(())
}

fn slot_id() -> String {
    env::var("SPUR_NOTEBOOK_SLOT_ID").unwrap_or_else(|_| "default".into())
}

enum Mode {
    App { headless: bool, files: Vec<PathBuf> },
    McpProxy { socket_path: PathBuf },
}

fn parse_mode_from(args: impl IntoIterator<Item = String>) -> Mode {
    let mut headless = false;
    let mut files = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--mcp-proxy" => {
                let socket_path = args
                    .next()
                    .map(PathBuf::from)
                    .unwrap_or_else(control_socket_path);
                return Mode::McpProxy { socket_path };
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
    Mode::App { headless, files }
}

fn parse_mode() -> Mode {
    parse_mode_from(env::args().skip(1))
}

async fn run_mcp_proxy(socket_path: PathBuf) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
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

fn main() {
    tracing_subscriber::fmt().init();

    let (headless, files) = match parse_mode() {
        Mode::App { headless, files } => (headless, files),
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

    let slot_id = slot_id();
    let socket_path = if headless {
        control_socket_path()
    } else {
        mcp::socket_path_for_slot(&slot_id).expect("failed to resolve notebook socket path")
    };
    let bridge = Arc::new(AgentBridge::new());
    let bridge_for_state = Arc::clone(&bridge);
    let bridge_for_setup = Arc::clone(&bridge);
    let bridge_for_run = Arc::clone(&bridge);
    let daemon_control = Arc::new(std::sync::OnceLock::new());
    let daemon_control_for_setup = Arc::clone(&daemon_control);
    let daemon_control_for_run = Arc::clone(&daemon_control);

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
                let result = if headless {
                    match mcp::start_daemon_server(server_socket_path, server_bridge, server_app)
                        .await
                    {
                        Ok((handle, control)) => {
                            let _ = daemon_control_for_setup.set(control);
                            Ok(handle)
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    mcp::start_server_with_app_bridge(server_socket_path, server_bridge, server_app)
                        .await
                };

                match result {
                    Ok(_handle) => std::future::pending::<()>().await,
                    Err(error) => tracing::error!(%error, "failed to start notebook MCP server"),
                }
            });

            if cfg!(any(windows, target_os = "linux")) {
                if headless {
                    return Ok(());
                } else if files.is_empty() {
                    jute::window::open_home(app.handle())?;
                } else {
                    handle_file_associations(app.handle(), &files)?;
                }
            }

            Ok(())
        })
        .menu(jute::menu::setup_menu)
        .build(tauri::generate_context!(
            "jute-notebook/src-tauri/tauri.conf.json"
        ))
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
                tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::CloseRequested { api, .. },
                    ..
                } if headless => {
                    api.prevent_close();
                    if let Some(control) = daemon_control_for_run.get().cloned() {
                        let label = label.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = control.hide_window_by_label(&label).await;
                        });
                    } else if let Some(window) = app.get_webview_window(&label) {
                        let _ = window.hide();
                    }
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
                    if !headless && app.webview_windows().is_empty() {
                        jute::window::open_home(app).unwrap();
                    }
                }
                _ => {}
            },
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        sync::{Mutex, OnceLock},
    };

    fn home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct HomeGuard(Option<OsString>);

    impl HomeGuard {
        fn set(home: &str) -> Self {
            let previous = env::var_os("HOME");
            env::set_var("HOME", home);
            Self(previous)
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => env::set_var("HOME", home),
                None => env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn mcp_proxy_default_socket_path_matches_core_for_fixed_home() {
        let _lock = home_lock().lock().expect("home lock poisoned");
        let _home = HomeGuard::set("/tmp/spur-notebook-home");

        let Mode::McpProxy { socket_path } = parse_mode_from(["--mcp-proxy".to_string()]) else {
            panic!("expected mcp proxy mode");
        };

        assert_eq!(socket_path, spur_core::notebook::control_socket_path());
        assert_eq!(
            socket_path,
            PathBuf::from("/tmp/spur-notebook-home/.spur/notebooks/control.sock")
        );
    }
}
