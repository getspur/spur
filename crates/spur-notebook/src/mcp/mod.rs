use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use rmcp::{
    model::{
        object as rmcp_object, CallToolRequestParams, CallToolResult, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RxJsonRpcMessage},
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Manager;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::oneshot,
    task::JoinHandle,
};
use tracing::warn;

use self::bridge::{BridgeRequester, TauriBridgeRequester};
use self::{
    bridge::{AgentBridge, BridgeError},
    transport::{read_frame_value, write_frame_json, LengthPrefixedJsonTransport},
};

pub mod bridge;
pub mod tools;
pub mod transport;

#[derive(Clone)]
pub struct NotebookMcpServer {
    bridge: Arc<dyn BridgeRequester>,
}

impl NotebookMcpServer {
    pub fn new(bridge: Arc<dyn BridgeRequester>) -> Self {
        Self { bridge }
    }

    fn tools(&self) -> Vec<Tool> {
        let mut all_tools = vec![Tool::new(
            "notebook.ping",
            "Smoke-test the SPUR notebook MCP socket.",
            rmcp_object(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
        )];
        all_tools.extend(self::tools::tools());
        all_tools
    }

    fn tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }
}

impl ServerHandler for NotebookMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions =
            Some("Use notebook tools to inspect and operate the active SPUR notebook.".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut implementation = Implementation::default();
        implementation.name = "notebook".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool(name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "notebook.ping" => Ok(CallToolResult::structured(json!({
                "ok": true,
                "tool": "notebook.ping",
                "listenerRegistered": self.bridge.listener_registered(),
                "windowAlive": self.bridge.window_alive()
            }))),
            "notebook.snapshot" => tools::snapshot::call(self.bridge.as_ref()).await,
            "notebook.read_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::read_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.kernel_info" => tools::kernel_info::call(self.bridge.as_ref()).await,
            "notebook.insert_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::insert_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.write_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::write_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.delete_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::delete_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.interrupt" => tools::interrupt::call(self.bridge.as_ref()).await,
            "notebook.run_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::run_cell::call(self.bridge.as_ref(), arguments, context).await
            }
            name => Err(McpError::invalid_params(
                format!("unknown notebook tool: {name}"),
                Some(json!({ "tool": name })),
            )),
        }
    }
}

pub struct NotebookMcpServerHandle {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl NotebookMcpServerHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.task).await;
        let _ = std::fs::remove_file(&self.socket_path);
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for NotebookMcpServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub fn socket_path_for_slot(slot_id: &str) -> Result<PathBuf> {
    if slot_id.is_empty() || slot_id.contains('/') || slot_id.contains('\0') {
        anyhow::bail!("invalid notebook slot id: {slot_id}");
    }
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join("notebooks")
        .join(format!("{slot_id}.sock")))
}

fn notebooks_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".spur").join("notebooks"))
}

fn last_notebook_record_path() -> Result<PathBuf> {
    Ok(notebooks_dir()?.join("last.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LastNotebookRecord {
    path: PathBuf,
}

async fn persist_last_notebook(path: &Path) -> Result<()> {
    persist_last_notebook_at(&last_notebook_record_path()?, path).await
}

async fn persist_last_notebook_at(record_path: &Path, path: &Path) -> Result<()> {
    if let Some(parent) = record_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let record = LastNotebookRecord {
        path: path.to_path_buf(),
    };
    let bytes = serde_json::to_vec_pretty(&record)?;
    let temp_path = record_path.with_file_name(format!(
        ".{}.{}.tmp",
        record_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("last.json"),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp_path, bytes)
        .await
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    tokio::fs::rename(&temp_path, record_path)
        .await
        .with_context(|| format!("failed to rename {}", record_path.display()))?;
    Ok(())
}

async fn load_last_notebook() -> Result<Option<PathBuf>> {
    load_last_notebook_at(&last_notebook_record_path()?).await
}

async fn load_last_notebook_at(record_path: &Path) -> Result<Option<PathBuf>> {
    let bytes = match tokio::fs::read(record_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", record_path.display()))
        }
    };
    let record: LastNotebookRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", record_path.display()))?;
    Ok(Some(record.path))
}

async fn clear_last_notebook() -> Result<()> {
    clear_last_notebook_at(&last_notebook_record_path()?).await
}

async fn clear_last_notebook_at(record_path: &Path) -> Result<()> {
    match tokio::fs::remove_file(record_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove {}", record_path.display()))
        }
    }
}

fn resolve_notebook_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

pub async fn start_server(socket_path: impl AsRef<Path>) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge(socket_path, Arc::new(AgentBridge::new())).await
}

pub async fn start_server_with_bridge(
    socket_path: impl AsRef<Path>,
    bridge: Arc<AgentBridge>,
) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge_requester(
        socket_path,
        Arc::new(TauriBridgeRequester::without_app(bridge)),
    )
    .await
}

pub async fn start_server_with_app_bridge(
    socket_path: impl AsRef<Path>,
    bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge_requester(
        socket_path,
        Arc::new(TauriBridgeRequester::with_app(bridge, app)),
    )
    .await
}

async fn start_server_with_bridge_requester(
    socket_path: impl AsRef<Path>,
    bridge: Arc<dyn BridgeRequester>,
) -> Result<NotebookMcpServerHandle> {
    let socket_path = socket_path.as_ref().to_path_buf();
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match tokio::fs::remove_file(&socket_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to remove stale socket {}", socket_path.display())
            });
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task_socket_path = socket_path.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            tracing::debug!(%error, "notebook MCP unix accept failed");
                            continue;
                        }
                    };
                    let bridge = Arc::clone(&bridge);
                    tokio::spawn(async move {
                        let transport = LengthPrefixedJsonTransport::<RoleServer>::new(stream);
                        match NotebookMcpServer::new(bridge).serve(transport).await {
                            Ok(running) => {
                                let _ = running.waiting().await;
                            }
                            Err(error) => {
                                tracing::debug!(%error, "notebook MCP session failed to initialize");
                            }
                        }
                    });
                }
            }
        }
        bridge.drain_on_shutdown().await;
        let _ = tokio::fs::remove_file(task_socket_path).await;
    });

    Ok(NotebookMcpServerHandle {
        socket_path,
        shutdown_tx: Some(shutdown_tx),
        task,
    })
}

#[derive(Clone)]
pub struct NotebookDaemonControl {
    bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
    state: Arc<tokio::sync::Mutex<NotebookDaemonState>>,
}

type DaemonControlFuture<'a> = Pin<Box<dyn Future<Output = DaemonControlResponse> + Send + 'a>>;

trait DaemonControlHandler: Clone + Send + 'static {
    fn handle_control<'a>(&'a self, request: DaemonControlRequest) -> DaemonControlFuture<'a>;
}

#[derive(Default)]
struct NotebookDaemonState {
    current_path: Option<PathBuf>,
    window_label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub daemon: Option<String>,
    pub command: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonControlError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlError {
    pub code: String,
    pub message: String,
}

impl NotebookDaemonControl {
    pub fn new(bridge: Arc<AgentBridge>, app: tauri::AppHandle) -> Self {
        Self {
            bridge,
            app,
            state: Arc::new(tokio::sync::Mutex::new(NotebookDaemonState::default())),
        }
    }

    async fn handle(&self, request: DaemonControlRequest) -> DaemonControlResponse {
        let id = request.id.clone();
        match self.handle_inner(request).await {
            Ok(path) => DaemonControlResponse {
                id,
                ok: true,
                path: path.map(|path| path.display().to_string()),
                error: None,
            },
            Err(error) => DaemonControlResponse {
                id,
                ok: false,
                path: None,
                error: Some(DaemonControlError {
                    code: error.mcp_code().to_string(),
                    message: error.to_string(),
                }),
            },
        }
    }

    async fn handle_inner(
        &self,
        request: DaemonControlRequest,
    ) -> Result<Option<PathBuf>, BridgeError> {
        match request.command.as_str() {
            "open" => {
                let path = request.path.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "open requires path".to_string(),
                })?;
                self.save_current().await?;
                self.open_path(resolve_notebook_path(path)).await.map(Some)
            }
            "new" => {
                self.save_current().await?;
                let path =
                    create_scratch_notebook()
                        .await
                        .map_err(|error| BridgeError::Handler {
                            code: "scratch_create_failed".to_string(),
                            message: error.to_string(),
                        })?;
                self.open_path(path).await.map(Some)
            }
            "reopen" => self.reopen().await.map(Some),
            "close" => {
                self.save_current().await?;
                self.close_current_window().await;
                self.bridge.set_notebook_open(false);
                let mut state = self.state.lock().await;
                state.current_path = None;
                state.window_label = None;
                if let Err(error) = clear_last_notebook().await {
                    warn!(%error, "failed to clear last notebook record");
                }
                Ok(None)
            }
            "shutdown" => {
                self.save_current().await?;
                self.app.exit(0);
                Ok(None)
            }
            command => Err(BridgeError::Handler {
                code: "unknown_daemon_command".to_string(),
                message: format!("unknown daemon command: {command}"),
            }),
        }
    }

    async fn save_current(&self) -> Result<(), BridgeError> {
        if !self.bridge.notebook_open() {
            return Ok(());
        }
        let requester = TauriBridgeRequester::with_app(Arc::clone(&self.bridge), self.app.clone());
        requester
            .request("notebook.save", json!({}), tools::BRIDGE_TIMEOUT)
            .await
            .map(|_| ())
    }

    async fn open_path(&self, path: PathBuf) -> Result<PathBuf, BridgeError> {
        self.close_current_window().await;
        self.bridge.set_notebook_open(false);
        {
            let mut state = self.state.lock().await;
            state.current_path = None;
            state.window_label = None;
        }
        let window = jute::window::open_notebook_path(&self.app, &path).map_err(|error| {
            BridgeError::Handler {
                code: "window_open_failed".to_string(),
                message: error.to_string(),
            }
        })?;
        let label = window.label().to_string();
        let mut state = self.state.lock().await;
        state.current_path = Some(path.clone());
        state.window_label = Some(label);
        if let Err(error) = persist_last_notebook(&path).await {
            warn!(%error, path = %path.display(), "failed to persist last notebook record");
        }
        Ok(path)
    }

    async fn reopen(&self) -> Result<PathBuf, BridgeError> {
        let (path, label) = {
            let state = self.state.lock().await;
            let path = state
                .current_path
                .clone()
                .ok_or(BridgeError::NotebookNotOpen)?;
            (path, state.window_label.clone())
        };

        if let Some(label) = label {
            if let Some(window) = self.app.get_webview_window(&label) {
                let _ = window.show();
                let _ = window.set_focus();
                return Ok(path);
            }
        }

        self.open_path(path).await
    }

    async fn close_current_window(&self) {
        let label = self.state.lock().await.window_label.clone();
        if let Some(label) = label {
            if let Some(window) = self.app.get_webview_window(&label) {
                let _ = window.destroy();
            }
        }
    }

    pub async fn hide_window_by_label(&self, label: &str) -> bool {
        let is_current = self
            .state
            .lock()
            .await
            .window_label
            .as_deref()
            .is_some_and(|current| current == label);
        if is_current {
            if let Some(window) = self.app.get_webview_window(label) {
                let _ = window.hide();
            }
        }
        is_current
    }

    pub async fn restore_last_open_notebook(&self) {
        match load_last_notebook().await {
            Ok(Some(path)) if path.exists() => {
                if let Err(error) = self.open_path(path.clone()).await {
                    warn!(
                        %error,
                        path = %path.display(),
                        "failed to restore last notebook"
                    );
                }
            }
            Ok(Some(path)) => {
                warn!(
                    path = %path.display(),
                    "last notebook no longer exists; clearing record"
                );
                if let Err(error) = clear_last_notebook().await {
                    warn!(%error, "failed to clear stale last notebook record");
                }
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "failed to load last notebook record"),
        }
    }
}

impl DaemonControlHandler for NotebookDaemonControl {
    fn handle_control<'a>(&'a self, request: DaemonControlRequest) -> DaemonControlFuture<'a> {
        Box::pin(async move { self.handle(request).await })
    }
}

async fn create_scratch_notebook() -> anyhow::Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    let dir = base_dirs.home_dir().join(".spur").join("scratch");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}.ipynb", uuid::Uuid::new_v4()));
    let contents = json!({
        "cells": [],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    });
    tokio::fs::write(&path, serde_json::to_vec_pretty(&contents)?).await?;
    Ok(path)
}

pub async fn start_daemon_server(
    socket_path: impl AsRef<Path>,
    bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
) -> Result<(NotebookMcpServerHandle, NotebookDaemonControl)> {
    let control = NotebookDaemonControl::new(Arc::clone(&bridge), app);
    let requester = Arc::new(TauriBridgeRequester::with_app(bridge, control.app.clone()));
    let handle = start_multiplexed_server(socket_path, requester, control.clone()).await?;
    control.restore_last_open_notebook().await;
    Ok((handle, control))
}

async fn start_multiplexed_server(
    socket_path: impl AsRef<Path>,
    bridge: Arc<dyn BridgeRequester>,
    control: NotebookDaemonControl,
) -> Result<NotebookMcpServerHandle> {
    let socket_path = prepare_socket_path(socket_path).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task_socket_path = socket_path.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            tracing::debug!(%error, "notebook daemon unix accept failed");
                            continue;
                        }
                    };
                    let bridge = Arc::clone(&bridge);
                    let control = control.clone();
                    tokio::spawn(async move {
                        handle_daemon_connection(stream, bridge, control).await;
                    });
                }
            }
        }
        let _ = tokio::fs::remove_file(task_socket_path).await;
    });

    Ok(NotebookMcpServerHandle {
        socket_path,
        shutdown_tx: Some(shutdown_tx),
        task,
    })
}

async fn handle_daemon_connection(
    mut stream: UnixStream,
    bridge: Arc<dyn BridgeRequester>,
    control: impl DaemonControlHandler,
) {
    let first = match read_frame_value(&mut stream).await {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(%error, "failed to read notebook daemon frame");
            return;
        }
    };

    if first.get("daemon").and_then(Value::as_str) == Some("notebook.v1") {
        let response = match serde_json::from_value::<DaemonControlRequest>(first) {
            Ok(request) => control.handle_control(request).await,
            Err(error) => DaemonControlResponse {
                id: None,
                ok: false,
                path: None,
                error: Some(DaemonControlError {
                    code: "invalid_control_message".to_string(),
                    message: error.to_string(),
                }),
            },
        };
        let _ = write_frame_json(&mut stream, &response).await;
        return;
    }

    if let Some(daemon) = first.get("daemon") {
        tracing::debug!(
            daemon = %daemon,
            "unknown notebook daemon discriminator; closing connection"
        );
        return;
    }

    let message = match serde_json::from_value::<RxJsonRpcMessage<RoleServer>>(first) {
        Ok(message) => message,
        Err(error) => {
            tracing::debug!(%error, "failed to decode initial notebook MCP frame");
            return;
        }
    };
    let transport =
        LengthPrefixedJsonTransport::<RoleServer>::with_initial_message(stream, message);
    match NotebookMcpServer::new(bridge).serve(transport).await {
        Ok(running) => {
            let _ = running.waiting().await;
        }
        Err(error) => {
            tracing::debug!(%error, "notebook MCP session failed to initialize");
        }
    }
}

async fn prepare_socket_path(socket_path: impl AsRef<Path>) -> Result<PathBuf> {
    let socket_path = socket_path.as_ref().to_path_buf();
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match tokio::fs::remove_file(&socket_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to remove stale socket {}", socket_path.display())
            });
        }
    }
    Ok(socket_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tokio::net::UnixStream;
    use tokio::time::{timeout, Duration};

    #[derive(Clone, Default)]
    struct RecordingDaemonControl {
        commands: Arc<StdMutex<Vec<String>>>,
    }

    impl RecordingDaemonControl {
        fn commands(&self) -> Vec<String> {
            self.commands.lock().expect("commands lock").clone()
        }
    }

    impl DaemonControlHandler for RecordingDaemonControl {
        fn handle_control<'a>(&'a self, request: DaemonControlRequest) -> DaemonControlFuture<'a> {
            Box::pin(async move {
                self.commands
                    .lock()
                    .expect("commands lock")
                    .push(request.command);
                DaemonControlResponse {
                    id: request.id,
                    ok: false,
                    path: None,
                    error: Some(DaemonControlError {
                        code: "recorded_control_request".to_string(),
                        message: "recorded control request".to_string(),
                    }),
                }
            })
        }
    }

    fn test_bridge_requester() -> Arc<dyn BridgeRequester> {
        Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        )))
    }

    fn spawn_multiplexer(control: RecordingDaemonControl) -> (UnixStream, JoinHandle<()>) {
        let (server, client) = UnixStream::pair().expect("in-memory stream pair");
        let handler = tokio::spawn(async move {
            handle_daemon_connection(server, test_bridge_requester(), control).await;
        });
        (client, handler)
    }

    fn initialize_frame() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "multiplexer-test",
                    "version": "0"
                }
            }
        })
    }

    async fn assert_connection_closes_without_response(mut client: UnixStream) {
        let read = timeout(Duration::from_millis(200), read_frame_value(&mut client))
            .await
            .expect("connection should close promptly");
        assert!(
            read.is_err(),
            "connection returned an unexpected frame: {read:?}"
        );
    }

    #[tokio::test]
    async fn multiplexer_routes_v1_daemon_frame_to_control_plane() {
        let control = RecordingDaemonControl::default();
        let (mut client, handler) = spawn_multiplexer(control.clone());

        write_frame_json(
            &mut client,
            &json!({
                "daemon": "notebook.v1",
                "command": "not-a-command"
            }),
        )
        .await
        .expect("control request writes");

        let response = read_frame_value(&mut client)
            .await
            .expect("control response reads");
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "recorded_control_request");
        assert_eq!(control.commands(), vec!["not-a-command"]);

        handler.await.expect("handler finishes");
    }

    #[tokio::test]
    async fn multiplexer_routes_json_rpc_frame_to_mcp_handler() {
        let control = RecordingDaemonControl::default();
        let (mut client, handler) = spawn_multiplexer(control.clone());

        write_frame_json(&mut client, &initialize_frame())
            .await
            .expect("initialize request writes");

        let response = read_frame_value(&mut client)
            .await
            .expect("initialize response reads");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["serverInfo"]["name"], "notebook");
        assert!(control.commands().is_empty());

        drop(client);
        handler.await.expect("handler finishes");
    }

    #[tokio::test]
    async fn multiplexer_closes_malformed_json_without_control_or_mcp_response() {
        let control = RecordingDaemonControl::default();
        let (mut client, handler) = spawn_multiplexer(control.clone());

        transport::write_frame_bytes(&mut client, b"{not-json")
            .await
            .expect("malformed frame writes");

        assert_connection_closes_without_response(client).await;
        assert!(control.commands().is_empty());
        handler.await.expect("handler finishes");
    }

    #[tokio::test]
    async fn multiplexer_closes_unknown_daemon_version_without_mcp_fallthrough() {
        let control = RecordingDaemonControl::default();
        let (mut client, handler) = spawn_multiplexer(control.clone());
        let mut frame = initialize_frame();
        frame["daemon"] = json!("notebook.v2");

        write_frame_json(&mut client, &frame)
            .await
            .expect("unknown daemon frame writes");

        assert_connection_closes_without_response(client).await;
        assert!(control.commands().is_empty());
        handler.await.expect("handler finishes");
    }

    #[tokio::test]
    async fn last_notebook_record_round_trips_and_clears() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-last-")
            .tempdir_in("/private/tmp")
            .expect("temp dir");
        let record_path = temp_dir.path().join("last.json");
        let notebook_path = temp_dir.path().join("analysis.ipynb");

        persist_last_notebook_at(&record_path, &notebook_path)
            .await
            .expect("record writes");

        let loaded = load_last_notebook_at(&record_path)
            .await
            .expect("record reads");
        assert_eq!(loaded.as_deref(), Some(notebook_path.as_path()));

        clear_last_notebook_at(&record_path)
            .await
            .expect("record clears");
        assert_eq!(
            load_last_notebook_at(&record_path)
                .await
                .expect("missing record reads as none"),
            None
        );
    }
}
