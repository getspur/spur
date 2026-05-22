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

use rmcp::{
    model::{ErrorCode, ErrorData as McpError},
    ErrorData,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Emitter;
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

pub type RequestId = Uuid;
pub type BridgeRequestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, BridgeError>> + Send + 'a>>;

pub trait BridgeRequester: Send + Sync {
    fn listener_registered(&self) -> bool;
    fn window_alive(&self) -> bool;

    fn request<'a>(
        &'a self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> BridgeRequestFuture<'a>;
}

#[derive(Debug, Clone)]
pub enum BridgeResponse {
    Success(Value),
    Error(BridgeError),
}

#[derive(Debug, Clone, Error)]
pub enum BridgeError {
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
            Self::WindowClosed | Self::AppRestarted => "app_restarted",
            Self::NoListener => "service_starting",
            Self::Timeout => "bridge_timeout",
            Self::Handler { code, .. } => code.as_str(),
        }
    }

    pub fn into_mcp_error(self) -> McpError {
        let code = match self {
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
            None => Box::pin(async { Err(BridgeError::NoListener) }),
        }
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
        }
    }

    pub fn listener_registered(&self) -> bool {
        self.listener_registered.load(Ordering::SeqCst)
    }

    pub fn window_alive(&self) -> bool {
        self.window_alive.load(Ordering::SeqCst)
    }

    pub fn mark_ready(&self) {
        self.window_alive.store(true, Ordering::SeqCst);
        self.listener_registered.store(true, Ordering::SeqCst);
    }

    pub async fn request<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        method: impl Into<String>,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
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
        self.drain_pending(BridgeError::WindowClosed).await;
    }

    pub async fn drain_on_shutdown(&self) {
        self.listener_registered.store(false, Ordering::SeqCst);
        self.window_alive.store(false, Ordering::SeqCst);
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
    bridge.mark_ready();
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
