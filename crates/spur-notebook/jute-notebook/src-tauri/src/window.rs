//! Shared code to open windows in Jute and notebooks.

use std::path::Path;

use anyhow::Context;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};
use uuid::Uuid;

// This produces equal 13px padding on the left and top of the window controls.
// The Figma prototype this is based on has 14px positioning, but this includes
// 1px of inset window border.
//
// The values come from the previous macOS traffic-light plugin. They produce
// the intended placement on macOS Sequoia.
#[cfg(target_os = "macos")]
const WINDOW_CONTROL_PAD_X: f64 = 12.0;
#[cfg(target_os = "macos")]
const WINDOW_CONTROL_PAD_Y: f64 = 17.0;

/// Initializes window size, min width, and other common settings on the
/// builder.
pub fn initialize_builder<'a, R: Runtime, M: Manager<R>>(
    manager: &'a M,
    path: &str,
) -> WebviewWindowBuilder<'a, R, M> {
    // Generate a unique window label since duplicates are not allowed.
    let label = format!("jute-window-{}", Uuid::new_v4());

    let url = tauri::WebviewUrl::App(path.trim_start_matches('/').into());

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(manager, &label, url)
        .title("Jute")
        .inner_size(960.0, 800.0)
        .min_inner_size(720.0, 600.0)
        .fullscreen(false)
        .resizable(true);

    #[cfg(target_os = "macos")]
    {
        // These methods are only available on macOS.
        builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay);
        builder = builder.hidden_title(true);
        builder = builder.traffic_light_position(tauri::LogicalPosition::new(
            WINDOW_CONTROL_PAD_X as f64,
            WINDOW_CONTROL_PAD_Y as f64,
        ));
    }

    builder
}

fn build_window<R: Runtime, M: Manager<R>>(
    builder: WebviewWindowBuilder<'_, R, M>,
) -> tauri::Result<WebviewWindow<R>> {
    let window = builder.build()?;
    attach_hide_on_close(&window);
    Ok(window)
}

#[cfg(target_os = "macos")]
fn attach_hide_on_close<R: Runtime>(window: &WebviewWindow<R>) {
    let hide_target = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = hide_target.hide();
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn attach_hide_on_close<R: Runtime>(_window: &WebviewWindow<R>) {}

/// Opens a window with the home page.
pub fn open_home<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    build_window(initialize_builder(app, "/"))
}

/// Opens a window with the notebook file at the given path.
pub fn open_notebook_path<R: Runtime>(
    app: &AppHandle<R>,
    file: &Path,
) -> tauri::Result<WebviewWindow<R>> {
    let query = serde_urlencoded::to_string([("path", file.to_string_lossy())])
        .context("could not encode path")?;
    build_window(initialize_builder(app, &format!("/notebook?{query}")))
}
