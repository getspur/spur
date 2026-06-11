//! Library code for the Jute application.

#![deny(unsafe_code)]
#![warn(missing_docs)]

extern crate self as spur_notebook;

use std::{io, sync::Arc};

use tauri::Emitter as _;
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

pub mod backend;
pub mod chat_commands;
pub mod chat_state;
pub mod commands;
pub mod entity;
pub mod identity;
/// Lazy provisioning of the bundled Python 3 kernelspec.
pub mod kernel_provision;
pub mod menu;
pub mod notebook_store;
pub mod ports;
pub mod sidebar_chat;
pub mod spur_app;
pub mod state;
pub mod window;

const NOTEBOOK_CHANGED_EVENT: &str = "notebook://changed";
const DATASOURCES_CHANGED_EVENT: &str = "datasources://changed";

/// Spawn the process-wide notebook delta forwarder.
///
/// The forwarder owns the single `broadcast::Receiver` for the process and emits
/// `notebook://changed` for every `NotebookDelta`.
pub fn spawn_notebook_delta_forwarder(app: tauri::AppHandle, state: Arc<state::State>) {
    let mut receiver = state.subscribe_notebook_deltas();
    tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(delta) => {
                    if let Err(error) = app.emit(NOTEBOOK_CHANGED_EVENT, delta) {
                        warn!(%error, "failed to emit notebook delta");
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped, "notebook delta receiver lagged");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Spawn the process-wide datasource catalog forwarder.
///
/// The forwarder owns a `broadcast::Receiver` for daemon events and emits
/// `datasources://changed` for every datasource catalog update.
pub fn spawn_datasources_changed_forwarder(app: tauri::AppHandle, state: Arc<state::State>) {
    let mut receiver = state.event_tx.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(state::DaemonEvent::DatasourcesChanged(entries)) => {
                    if let Err(error) = app.emit(DATASOURCES_CHANGED_EVENT, entries) {
                        warn!(%error, "failed to emit datasource catalog update");
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped, "datasource catalog receiver lagged");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// A serializable error type for application errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An error occurred while starting or managing a subprocess.
    #[error("failed to run subprocess: {0}")]
    Subprocess(io::Error),

    /// Could not connect to the kernel.
    #[error("could not connect to the kernel: {0}")]
    KernelConnect(String),

    /// Could not provision the local Python kernel.
    #[error("could not provision local Python kernel during {stage}: {cause}")]
    KernelProvisionFailed {
        /// Provisioning stage that failed.
        stage: &'static str,
        /// Underlying error text for the failed stage.
        cause: String,
    },

    /// Could not inject the SPUR port bootstrap into a fresh kernel.
    #[error("could not inject SPUR port bootstrap during {stage}: {cause}")]
    PortBootstrapFailed {
        /// Bootstrap stage that failed.
        stage: &'static str,
        /// Underlying error text for the failed stage.
        cause: String,
    },

    /// Disconnected while communicating with a kernel.
    #[error("disconnected from the kernel")]
    KernelDisconnect,

    /// Could not find the kernel.
    #[error("kernel not found")]
    KernelNotFound,

    /// Could not find the kernel process.
    #[error("kernel process not found")]
    KernelProcessNotFound,

    /// An invalid URL was provided or constructed.
    #[error("invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// HTTP error from reqwest while making a request.
    #[error("HTTP failure: {0}")]
    ReqwestError(#[from] reqwest::Error),

    /// Error while deserializing a message.
    #[error("could not deserialize message: {0}")]
    DeserializeMessage(String),

    /// Error originating from `ZeroMQ`.
    #[error("zeromq: {0}")]
    Zmq(#[from] zeromq::ZmqError),

    /// Error originating from `serde_json`.
    #[error("serde_json error: {0}")]
    SerdeJson(#[from] serde_json::error::Error),

    /// Error interacting with the filesystem.
    #[error("filesystem error: {0}")]
    Filesystem(io::Error),

    /// Error returned directly from Tauri.
    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),

    /// Error while interacting with the shell plugin.
    #[error("shell plugin error: {0}")]
    PluginShell(#[from] tauri_plugin_shell::Error),

    /// Error while interacting with the notebook daemon control protocol.
    #[error("notebook daemon error: {0}")]
    NotebookDaemon(String),
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}
