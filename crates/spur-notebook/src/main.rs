// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{env, path::PathBuf, sync::Arc};

use jute::state::State;
use spur_notebook::mcp::{self, bridge::AgentBridge};
use tauri::{AppHandle, Manager};

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

fn main() {
    tracing_subscriber::fmt().init();

    let slot_id = slot_id();
    let socket_path =
        mcp::socket_path_for_slot(&slot_id).expect("failed to resolve notebook socket path");
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
            jute::commands::get_notebook,
            jute::commands::save_to_disk,
            jute::commands::venv::venv_list_python_versions,
            jute::commands::venv::venv_create,
            jute::commands::venv::venv_list,
            jute::commands::venv::venv_delete,
            spur_notebook::mcp::bridge::bridge_ready,
            spur_notebook::mcp::bridge::agent_response,
        ])
        .setup(move |app| {
            let server_bridge = Arc::clone(&bridge_for_setup);
            let server_socket_path = socket_path.clone();
            let server_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match mcp::start_server_with_app_bridge(
                    server_socket_path,
                    server_bridge,
                    server_app,
                )
                .await
                {
                    Ok(_handle) => std::future::pending::<()>().await,
                    Err(error) => tracing::error!(%error, "failed to start notebook MCP server"),
                }
            });

            if cfg!(any(windows, target_os = "linux")) {
                let mut files = Vec::new();

                for maybe_file in env::args().skip(1) {
                    if maybe_file.starts_with('-') {
                        continue;
                    }
                    if let Ok(url) = url::Url::parse(&maybe_file) {
                        if url.scheme() == "file" {
                            if let Ok(path) = url.to_file_path() {
                                files.push(path);
                            }
                        }
                    } else {
                        files.push(PathBuf::from(maybe_file));
                    }
                }

                if files.is_empty() {
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
                    if app.webview_windows().is_empty() {
                        jute::window::open_home(app).unwrap();
                    }
                }
                _ => {}
            },
        );
}
