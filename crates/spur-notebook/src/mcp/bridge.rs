use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use jute::backend::commands::RunCellEvent;
use rmcp::{
    model::{ErrorCode, ErrorData as McpError},
    ErrorData,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Emitter;
use tauri::Manager;
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

pub type RequestId = Uuid;
pub type BridgeRequestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, BridgeError>> + Send + 'a>>;
pub type RunCellEventFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<async_channel::Receiver<RunCellEvent>, BridgeError>> + Send + 'a,
    >,
>;

pub trait BridgeRequester: Send + Sync {
    fn listener_registered(&self) -> bool;
    fn window_alive(&self) -> bool;
    fn notebook_open(&self) -> bool;

    fn request<'a>(
        &'a self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> BridgeRequestFuture<'a>;

    fn run_cell_events<'a>(
        &'a self,
        _kernel_id: &'a str,
        _code: &'a str,
    ) -> RunCellEventFuture<'a> {
        Box::pin(async {
            Err(BridgeError::Handler {
                code: "kernel_unavailable".to_string(),
                message: "notebook kernel access is unavailable".to_string(),
            })
        })
    }

    fn drain_on_shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

#[derive(Debug, Clone)]
pub enum BridgeResponse {
    Success(Value),
    Error(BridgeError),
}

#[derive(Debug, Clone, Error)]
pub enum BridgeError {
    #[error("no notebook is loaded")]
    NotebookNotOpen,
    #[error("notebook window is closed")]
    WindowClosed,
    #[error("notebook app restarted")]
    AppRestarted,
    #[error("notebook bridge listener is not ready")]
    NoListener,
    #[error("notebook bridge request timed out")]
    Timeout,
    #[error("notebook handler error {code}: {message}")]
    Handler { code: String, message: String },
}

impl BridgeError {
    pub fn mcp_code(&self) -> &str {
        match self {
            Self::NotebookNotOpen => "notebook_not_open",
            Self::WindowClosed | Self::AppRestarted => "app_restarted",
            Self::NoListener => "notebook_not_open",
            Self::Timeout => "bridge_timeout",
            Self::Handler { code, .. } => code.as_str(),
        }
    }

    pub fn into_mcp_error(self) -> McpError {
        let code = match self {
            Self::NotebookNotOpen => -32060,
            Self::NoListener => -32061,
            Self::Timeout => -32062,
            Self::WindowClosed | Self::AppRestarted => -32063,
            Self::Handler { .. } => -32064,
        };
        let mcp_code = self.mcp_code().to_string();
        ErrorData::new(
            ErrorCode(code),
            self.to_string(),
            Some(json!({ "code": mcp_code })),
        )
    }
}

fn kernel_bridge_error(error: jute::Error) -> BridgeError {
    let code = match &error {
        jute::Error::KernelDisconnect => "kernel_disconnected",
        jute::Error::KernelNotFound => "kernel_not_found",
        jute::Error::KernelProcessNotFound => "kernel_process_not_found",
        _ => "kernel_error",
    };
    BridgeError::Handler {
        code: code.to_string(),
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRequest {
    pub request_id: RequestId,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeResponsePayload {
    pub request_id: RequestId,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<BridgeHandlerError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHandlerError {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct AgentBridge {
    pending: Mutex<HashMap<RequestId, oneshot::Sender<BridgeResponse>>>,
    listener_registered: AtomicBool,
    window_alive: AtomicBool,
    notebook_open: AtomicBool,
}

pub struct TauriBridgeRequester {
    bridge: Arc<AgentBridge>,
    app: Option<tauri::AppHandle>,
}

impl TauriBridgeRequester {
    pub fn without_app(bridge: Arc<AgentBridge>) -> Self {
        Self { bridge, app: None }
    }

    pub fn with_app(bridge: Arc<AgentBridge>, app: tauri::AppHandle) -> Self {
        Self {
            bridge,
            app: Some(app),
        }
    }
}

impl BridgeRequester for TauriBridgeRequester {
    fn listener_registered(&self) -> bool {
        self.bridge.listener_registered()
    }

    fn window_alive(&self) -> bool {
        self.bridge.window_alive()
    }

    fn notebook_open(&self) -> bool {
        self.bridge.notebook_open()
    }

    fn request<'a>(
        &'a self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        match &self.app {
            Some(app) => {
                Box::pin(async move { self.bridge.request(app, method, params, timeout).await })
            }
            None => Box::pin(async { Err(BridgeError::NotebookNotOpen) }),
        }
    }

    fn run_cell_events<'a>(&'a self, kernel_id: &'a str, code: &'a str) -> RunCellEventFuture<'a> {
        if !self.bridge.notebook_open() {
            return Box::pin(async { Err(BridgeError::NotebookNotOpen) });
        }
        match &self.app {
            Some(app) => Box::pin(async move {
                let state = app.state::<jute::state::State>();
                jute::commands::run_cell_events(kernel_id, code, &state)
                    .await
                    .map_err(kernel_bridge_error)
            }),
            None => Box::pin(async { Err(BridgeError::NotebookNotOpen) }),
        }
    }

    fn drain_on_shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let bridge = Arc::clone(&self.bridge);
        Box::pin(async move { bridge.drain_on_shutdown().await })
    }
}

impl Default for AgentBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentBridge {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            listener_registered: AtomicBool::new(false),
            window_alive: AtomicBool::new(true),
            notebook_open: AtomicBool::new(false),
        }
    }

    pub fn listener_registered(&self) -> bool {
        self.listener_registered.load(Ordering::SeqCst)
    }

    pub fn window_alive(&self) -> bool {
        self.window_alive.load(Ordering::SeqCst)
    }

    pub fn notebook_open(&self) -> bool {
        self.notebook_open.load(Ordering::SeqCst)
    }

    pub async fn mark_ready(&self) {
        self.drain_pending(BridgeError::AppRestarted).await;
        self.window_alive.store(true, Ordering::SeqCst);
        self.listener_registered.store(true, Ordering::SeqCst);
    }

    pub fn set_notebook_open(&self, open: bool) {
        self.notebook_open.store(open, Ordering::SeqCst);
    }

    pub async fn request<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        method: impl Into<String>,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        if !self.notebook_open() {
            return Err(BridgeError::NotebookNotOpen);
        }
        if !self.window_alive() {
            return Err(BridgeError::WindowClosed);
        }
        if !self.listener_registered() {
            return Err(BridgeError::NoListener);
        }

        let request_id = RequestId::new_v4();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id, tx);

        let payload = BridgeRequest {
            request_id,
            method: method.into(),
            params,
        };

        if app.emit("agent://request", payload).is_err() {
            self.pending.lock().await.remove(&request_id);
            return Err(BridgeError::WindowClosed);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(BridgeResponse::Success(value))) => Ok(value),
            Ok(Ok(BridgeResponse::Error(error))) => Err(error),
            Ok(Err(_closed)) => Err(BridgeError::AppRestarted),
            Err(_elapsed) => {
                self.pending.lock().await.remove(&request_id);
                Err(BridgeError::Timeout)
            }
        }
    }

    pub async fn respond(&self, payload: BridgeResponsePayload) -> Result<(), BridgeError> {
        let Some(sender) = self.pending.lock().await.remove(&payload.request_id) else {
            return Err(BridgeError::AppRestarted);
        };

        let response = match payload.error {
            Some(error) => BridgeResponse::Error(BridgeError::Handler {
                code: error.code,
                message: error.message,
            }),
            None => BridgeResponse::Success(payload.result.unwrap_or(Value::Null)),
        };

        let _ = sender.send(response);
        Ok(())
    }

    pub async fn mark_window_closed(&self) {
        self.listener_registered.store(false, Ordering::SeqCst);
        self.window_alive.store(false, Ordering::SeqCst);
        self.notebook_open.store(false, Ordering::SeqCst);
        self.drain_pending(BridgeError::WindowClosed).await;
    }

    pub async fn drain_on_shutdown(&self) {
        self.listener_registered.store(false, Ordering::SeqCst);
        self.window_alive.store(false, Ordering::SeqCst);
        self.notebook_open.store(false, Ordering::SeqCst);
        self.drain_pending(BridgeError::AppRestarted).await;
    }

    async fn drain_pending(&self, error: BridgeError) {
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for sender in pending.into_values() {
            let _ = sender.send(BridgeResponse::Error(error.clone()));
        }
    }
}

#[tauri::command]
pub async fn bridge_ready(bridge: tauri::State<'_, Arc<AgentBridge>>) -> Result<(), String> {
    bridge.mark_ready().await;
    Ok(())
}

#[tauri::command]
pub async fn notebook_active_changed(
    bridge: tauri::State<'_, Arc<AgentBridge>>,
    open: bool,
) -> Result<(), String> {
    bridge.set_notebook_open(open);
    Ok(())
}

#[tauri::command]
pub async fn agent_response(
    bridge: tauri::State<'_, Arc<AgentBridge>>,
    payload: BridgeResponsePayload,
) -> Result<(), String> {
    bridge
        .respond(payload)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_errors_map_to_expected_mcp_codes() {
        let cases: &[(BridgeError, i32, &str)] = &[
            (
                BridgeError::NotebookNotOpen,
                -32060,
                "no notebook is loaded",
            ),
            (
                BridgeError::WindowClosed,
                -32063,
                "notebook window is closed",
            ),
            (BridgeError::AppRestarted, -32063, "notebook app restarted"),
            (
                BridgeError::NoListener,
                -32061,
                "notebook bridge listener is not ready",
            ),
            (
                BridgeError::Timeout,
                -32062,
                "notebook bridge request timed out",
            ),
            (
                BridgeError::Handler {
                    code: "handler_failed".to_string(),
                    message: "boom".to_string(),
                },
                -32064,
                "notebook handler error handler_failed: boom",
            ),
        ];

        for (error, expected_code, expected_message_prefix) in cases {
            let mcp_error = error.clone().into_mcp_error();

            assert_eq!(mcp_error.code, ErrorCode(*expected_code));
            assert!(
                mcp_error.message.starts_with(expected_message_prefix),
                "expected message {:?} to start with {:?}",
                mcp_error.message,
                expected_message_prefix
            );
        }
    }

    #[tokio::test]
    async fn bridge_ready_drains_pending_requests_as_app_restarted() {
        let bridge = AgentBridge::new();
        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();

        bridge
            .pending
            .lock()
            .await
            .insert(RequestId::new_v4(), tx_a);
        bridge
            .pending
            .lock()
            .await
            .insert(RequestId::new_v4(), tx_b);

        bridge.mark_ready().await;

        for rx in [rx_a, rx_b] {
            match rx.await.expect("pending request should resolve") {
                BridgeResponse::Error(BridgeError::AppRestarted) => {}
                other => panic!("expected AppRestarted, got {other:?}"),
            }
        }

        assert!(bridge.pending.lock().await.is_empty());
        assert!(bridge.listener_registered());
        assert!(bridge.window_alive());

        let error = BridgeError::AppRestarted.into_mcp_error();
        assert_eq!(error.data.unwrap()["code"], "app_restarted");
    }
}
