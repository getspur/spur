use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use jute::{backend::notebook::NotebookRoot, state::State};
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
use tauri::{Emitter, Manager};
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    sync::oneshot,
    task::JoinHandle,
};
use tracing::warn;

use crate::recents::{self, RecentEntry};

use self::bridge::{BridgeRequester, TauriBridgeRequester};
use self::loopback_requester::LoopbackDaemonRequester;
use self::{
    bridge::{AgentBridge, BridgeError},
    transport::{read_frame_value, write_frame_json, LengthPrefixedJsonTransport},
};

const FLUSH_PENDING_TIMEOUT: Duration = Duration::from_secs(2);
const NOTEBOOK_LOAD_READY_POLL: Duration = Duration::from_millis(25);

pub mod bridge;
pub mod loopback_requester;
pub mod tools;
pub mod transport;

/// Shared dependencies plumbed into every MCP tool invocation. Future tools
/// will reach for `state` (kernel slots, save coordinator) and `app` (fan-out
/// `Emitter::emit`); the existing five tools only use `bridge`. `state` and
/// `app` are `Option` so non-daemon entry points (`start_server`, unit tests)
/// can construct a `ServerDeps` without a live Tauri runtime.
pub struct ServerDeps {
    pub bridge: Arc<dyn BridgeRequester>,
    pub state: Option<Arc<State>>,
    pub app: Option<tauri::AppHandle>,
    /// In-process daemon control plane. Populated only by `start_daemon_server`;
    /// daemon-routed MCP tools surface `daemon_unavailable` when this is `None`.
    pub daemon: Option<NotebookDaemonControl>,
}

impl ServerDeps {
    /// Build a `ServerDeps` carrying only a bridge — used by the standalone
    /// `start_server` path and by tool-level unit tests that exercise the
    /// bridge contract directly.
    pub fn from_bridge(bridge: Arc<dyn BridgeRequester>) -> Self {
        Self {
            bridge,
            state: None,
            app: None,
            daemon: None,
        }
    }
}

#[derive(Clone)]
pub struct NotebookMcpServer {
    deps: Arc<ServerDeps>,
}

impl NotebookMcpServer {
    pub fn new(deps: Arc<ServerDeps>) -> Self {
        Self { deps }
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        fn arguments(arguments: Option<serde_json::Map<String, Value>>) -> Value {
            arguments.map(Value::Object).unwrap_or_else(|| json!({}))
        }

        let name = request.name;
        let arguments = arguments(request.arguments);

        match name.as_ref() {
            "notebook.ping" => Ok(CallToolResult::structured(json!({
                "ok": true,
                "tool": "notebook.ping",
                "listenerRegistered": self.deps.bridge.listener_registered(),
                "windowAlive": self.deps.bridge.window_alive()
            }))),
            "notebook.snapshot" => tools::snapshot::call(&self.deps).await,
            "notebook.get_notebook" => tools::get_notebook::call(&self.deps, arguments).await,
            "notebook.read_cell" => tools::read_cell::call(&self.deps, arguments).await,
            "notebook.kernel_info" => tools::kernel_info::call(&self.deps, arguments).await,
            "notebook.insert_cell" => tools::insert_cell::call(&self.deps, arguments).await,
            "notebook.write_cell" => tools::write_cell::call(&self.deps, arguments).await,
            "notebook.save" => tools::save::call(&self.deps, arguments).await,
            "notebook.delete_cell" => tools::delete_cell::call(&self.deps, arguments).await,
            "notebook.interrupt" => tools::interrupt::call(&self.deps, arguments).await,
            "notebook.run_cell" => tools::run_cell::call(&self.deps, arguments).await,
            "notebook.start_kernel" => tools::start_kernel::call(&self.deps, arguments).await,
            "notebook.restart_kernel" => tools::restart_kernel::call(&self.deps, arguments).await,
            "notebook.stop_kernel" => tools::stop_kernel::call(&self.deps, arguments).await,
            "notebook.venv_list" => tools::venv_list::call(&self.deps, arguments).await,
            "notebook.venv_create" => tools::venv_create::call(&self.deps, arguments).await,
            "notebook.venv_delete" => tools::venv_delete::call(&self.deps, arguments).await,
            "notebook.venv_list_python_versions" => {
                tools::venv_list_python_versions::call(&self.deps, arguments).await
            }
            "notebook.new" => tools::daemon_lifecycle::call_new(&self.deps, arguments).await,
            "notebook.open" => tools::daemon_lifecycle::call_open(&self.deps, arguments).await,
            "notebook.close" => tools::daemon_lifecycle::call_close(&self.deps, arguments).await,
            "notebook.reopen" => tools::daemon_lifecycle::call_reopen(&self.deps, arguments).await,
            "notebook.list_recents" => {
                tools::daemon_recents::call_list_recents(&self.deps, arguments).await
            }
            "notebook.set_pinned" => {
                tools::daemon_recents::call_set_pinned(&self.deps, arguments).await
            }
            "notebook.remove_from_recents" => {
                tools::daemon_recents::call_remove_from_recents(&self.deps, arguments).await
            }
            "notebook.move_to_trash" => {
                tools::daemon_files::call_move_to_trash(&self.deps, arguments).await
            }
            "notebook.reveal_in_finder" => {
                tools::daemon_files::call_reveal_in_finder(&self.deps, arguments).await
            }
            "notebook.discard_scratch" => {
                tools::daemon_files::call_discard_scratch(&self.deps, arguments).await
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotebookConfig {
    pub in_proc_store: bool,
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

fn notebooks_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".spur").join("notebooks"))
}

fn scratch_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".spur").join("scratch"))
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

async fn start_server_with_bridge_requester(
    socket_path: impl AsRef<Path>,
    bridge: Arc<dyn BridgeRequester>,
) -> Result<NotebookMcpServerHandle> {
    let deps = Arc::new(ServerDeps::from_bridge(Arc::clone(&bridge)));
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
                    let deps = Arc::clone(&deps);
                    tokio::spawn(async move {
                        let transport = LengthPrefixedJsonTransport::<RoleServer>::new(stream);
                        match NotebookMcpServer::new(deps).serve(transport).await {
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
    requester: Arc<dyn BridgeRequester>,
    jute_state: Arc<State>,
    windows: Arc<dyn DaemonWindowOps>,
    state: Arc<tokio::sync::Mutex<NotebookDaemonState>>,
    last_record_path: Option<PathBuf>,
    recents_record_path: Option<PathBuf>,
}

#[doc(hidden)]
pub trait DaemonWindowOps: Send + Sync {
    fn show_and_focus(&self, label: &str) -> bool;
    fn hide(&self, label: &str);
    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError>;
    fn emit_recents_changed(&self);
    fn exit(&self);
}

struct TauriDaemonWindowOps {
    app: tauri::AppHandle,
}

impl DaemonWindowOps for TauriDaemonWindowOps {
    fn show_and_focus(&self, label: &str) -> bool {
        let Some(window) = self.app.get_webview_window(label) else {
            return false;
        };
        let _ = window.show();
        let _ = window.set_focus();
        true
    }

    fn hide(&self, label: &str) {
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.hide();
        }
    }

    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
        let window = jute::window::open_notebook_path(&self.app, path).map_err(|error| {
            BridgeError::Handler {
                code: "window_open_failed".to_string(),
                message: error.to_string(),
            }
        })?;
        Ok(window.label().to_string())
    }

    fn emit_recents_changed(&self) {
        let _ = self
            .app
            .emit(self::tools::RECENTS_CHANGED_EVENT, &json!({}));
    }

    fn exit(&self) {
        self.app.exit(0);
    }
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub daemon: Option<String>,
    pub command: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub from: Option<PathBuf>,
    #[serde(default)]
    pub to: Option<PathBuf>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, alias = "expected_version")]
    pub expected_version: Option<u64>,
    #[serde(default, alias = "last_edited_by")]
    pub last_edited_by: Option<String>,
    #[serde(default)]
    pub kind: Option<jute::notebook_store::CellKind>,
    #[serde(default, alias = "after_id")]
    pub after_id: Option<String>,
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
    pub entries: Option<Vec<RecentEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DaemonControlError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct PendingNotebookFlush {
    path: PathBuf,
    contents: NotebookRoot,
}

struct DaemonControlSuccess {
    path: Option<PathBuf>,
    entries: Option<Vec<RecentEntry>>,
}

impl DaemonControlSuccess {
    fn empty() -> Self {
        Self {
            path: None,
            entries: None,
        }
    }

    fn path(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            entries: None,
        }
    }

    fn entries(entries: Vec<RecentEntry>) -> Self {
        Self {
            path: None,
            entries: Some(entries),
        }
    }
}

impl NotebookDaemonControl {
    pub fn new(bridge: Arc<AgentBridge>, app: tauri::AppHandle, jute_state: Arc<State>) -> Self {
        let requester: Arc<dyn BridgeRequester> = Arc::new(TauriBridgeRequester::with_app(
            Arc::clone(&bridge),
            app.clone(),
        ));
        let windows: Arc<dyn DaemonWindowOps> = Arc::new(TauriDaemonWindowOps { app });
        Self::new_with_parts(bridge, requester, jute_state, windows, None)
    }

    fn new_with_parts(
        bridge: Arc<AgentBridge>,
        requester: Arc<dyn BridgeRequester>,
        jute_state: Arc<State>,
        windows: Arc<dyn DaemonWindowOps>,
        last_record_path: Option<PathBuf>,
    ) -> Self {
        Self {
            bridge,
            requester,
            jute_state,
            windows,
            state: Arc::new(tokio::sync::Mutex::new(NotebookDaemonState::default())),
            last_record_path,
            recents_record_path: None,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        bridge: Arc<AgentBridge>,
        requester: Arc<dyn BridgeRequester>,
        jute_state: Arc<State>,
        windows: Arc<dyn DaemonWindowOps>,
        last_record_path: Option<PathBuf>,
    ) -> Self {
        Self::new_with_parts(bridge, requester, jute_state, windows, last_record_path)
    }

    #[cfg(test)]
    fn new_for_test_with_recents_record(
        bridge: Arc<AgentBridge>,
        requester: Arc<dyn BridgeRequester>,
        jute_state: Arc<State>,
        windows: Arc<dyn DaemonWindowOps>,
        last_record_path: Option<PathBuf>,
        recents_record_path: Option<PathBuf>,
    ) -> Self {
        let mut control =
            Self::new_with_parts(bridge, requester, jute_state, windows, last_record_path);
        control.recents_record_path = recents_record_path;
        control
    }

    #[doc(hidden)]
    pub fn new_with_parts_for_test(
        bridge: Arc<AgentBridge>,
        requester: Arc<dyn BridgeRequester>,
        jute_state: Arc<State>,
        windows: Arc<dyn DaemonWindowOps>,
        last_record_path: Option<PathBuf>,
    ) -> Self {
        Self::new_with_parts(bridge, requester, jute_state, windows, last_record_path)
    }

    pub async fn handle(&self, request: DaemonControlRequest) -> DaemonControlResponse {
        if is_notebook_store_command(&request.command) {
            return self.handle_notebook_store_control(request).await;
        }

        let id = request.id.clone();
        match self.handle_inner(request).await {
            Ok(success) => DaemonControlResponse {
                id,
                ok: true,
                path: success.path.map(|path| path.display().to_string()),
                entries: success.entries,
                result: None,
                error: None,
            },
            Err(error) => DaemonControlResponse {
                id,
                ok: false,
                path: None,
                entries: None,
                result: None,
                error: Some(DaemonControlError {
                    code: error.mcp_code().to_string(),
                    message: error.to_string(),
                }),
            },
        }
    }

    async fn handle_notebook_store_control(
        &self,
        request: DaemonControlRequest,
    ) -> DaemonControlResponse {
        let cell_id = request.id.clone();
        let jute_request = match notebook_store_request_from_daemon(request) {
            Ok(request) => request,
            Err(error) => {
                return DaemonControlResponse {
                    id: cell_id,
                    ok: false,
                    path: None,
                    entries: None,
                    result: None,
                    error: Some(error),
                }
            }
        };
        let response =
            jute::commands::handle_daemon_control_request(jute_request, &self.jute_state).await;
        DaemonControlResponse {
            id: cell_id,
            ok: response.ok,
            path: response.path,
            entries: None,
            result: response
                .result
                .and_then(|result| serde_json::to_value(result).ok()),
            error: response.error.map(|error| DaemonControlError {
                code: error.code,
                message: error.message,
            }),
        }
    }

    async fn handle_inner(
        &self,
        request: DaemonControlRequest,
    ) -> Result<DaemonControlSuccess, BridgeError> {
        match request.command.as_str() {
            "open" => {
                let path = request.path.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "open requires path".to_string(),
                })?;
                self.save_current().await?;
                let path = self.open_path(resolve_notebook_path(path)).await?;
                if let Err(error) = self.record_recent_open(&path).await {
                    warn!(%error, path = %path.display(), "failed to record recent notebook");
                } else {
                    self.windows.emit_recents_changed();
                }
                Ok(DaemonControlSuccess::path(path))
            }
            "new" => {
                self.save_current().await?;
                let path =
                    create_untitled_notebook()
                        .await
                        .map_err(|error| BridgeError::Handler {
                            code: "scratch_create_failed".to_string(),
                            message: error.to_string(),
                        })?;
                let path = self.open_path(path).await?;
                if let Err(error) = self.record_recent_open(&path).await {
                    warn!(%error, path = %path.display(), "failed to record recent notebook");
                } else {
                    self.windows.emit_recents_changed();
                }
                Ok(DaemonControlSuccess::path(path))
            }
            "reopen" => self.reopen().await.map(DaemonControlSuccess::path),
            "rename" => {
                let from = request.from.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "rename requires from".to_string(),
                })?;
                let to = request.to.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "rename requires to".to_string(),
                })?;
                self.save_current().await?;
                let path = self
                    .rename_path(resolve_notebook_path(from), resolve_notebook_path(to))
                    .await?;
                Ok(DaemonControlSuccess::path(path))
            }
            "close" => {
                self.save_current().await?;
                self.close_current_window().await;
                self.bridge.set_notebook_open(false);
                {
                    let mut state = self.state.lock().await;
                    state.current_path = None;
                    state.window_label = None;
                }
                if let Err(error) = self.clear_last_notebook().await {
                    warn!(%error, "failed to clear last notebook record");
                }
                self.windows.emit_recents_changed();
                Ok(DaemonControlSuccess::empty())
            }
            "list_recents" => recents::list_recents()
                .await
                .map(DaemonControlSuccess::entries)
                .map_err(|error| BridgeError::Handler {
                    code: "recents_failed".to_string(),
                    message: error.to_string(),
                }),
            "remove_from_recents" => {
                let path = request.path.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "remove_from_recents requires path".to_string(),
                })?;
                recents::remove_from_recents(&resolve_notebook_path(path))
                    .await
                    .map_err(|error| BridgeError::Handler {
                        code: "recents_failed".to_string(),
                        message: error.to_string(),
                    })?;
                self.windows.emit_recents_changed();
                Ok(DaemonControlSuccess::empty())
            }
            "set_pinned" => {
                let path = request.path.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "set_pinned requires path".to_string(),
                })?;
                let pinned = request.pinned.ok_or_else(|| BridgeError::Handler {
                    code: "invalid_params".to_string(),
                    message: "set_pinned requires pinned".to_string(),
                })?;
                recents::set_pinned(&resolve_notebook_path(path), pinned)
                    .await
                    .map_err(|error| BridgeError::Handler {
                        code: "recents_failed".to_string(),
                        message: error.to_string(),
                    })?;
                self.windows.emit_recents_changed();
                Ok(DaemonControlSuccess::empty())
            }
            "shutdown" => {
                self.save_current().await?;
                self.windows.exit();
                Ok(DaemonControlSuccess::empty())
            }
            command => Err(BridgeError::Handler {
                code: "unknown_daemon_command".to_string(),
                message: format!("unknown daemon command: {command}"),
            }),
        }
    }

    async fn save_current(&self) -> Result<(), BridgeError> {
        let path = {
            let state = self.state.lock().await;
            state.current_path.clone()
        };
        let Some(path) = path else {
            return Ok(());
        };
        if !self.requester.notebook_open() {
            return Ok(());
        }

        let value = match self.requester.flush_pending(FLUSH_PENDING_TIMEOUT).await {
            Ok(value) => value,
            Err(error) if should_continue_without_flush(&error) => {
                warn!(
                    %error,
                    path = %path.display(),
                    "failed to flush pending notebook edits before lifecycle transition; proceeding"
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if value.is_null() {
            return Ok(());
        }
        let flush: PendingNotebookFlush =
            serde_json::from_value(value).map_err(|error| BridgeError::Handler {
                code: "invalid_notebook_flush".to_string(),
                message: error.to_string(),
            })?;

        self.jute_state
            .save_coordinator
            .save(flush.path, flush.contents)
            .await
            .map_err(|error| BridgeError::Handler {
                code: "save_failed".to_string(),
                message: error.to_string(),
            })
    }

    async fn record_recent_open(&self, path: &Path) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::record_open_at(record_path, &scratch_dir()?, path).await,
            None => recents::record_open(path).await,
        }
    }

    async fn rename_recent_path(&self, from: &Path, to: &Path) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::rename_path_at(record_path, from, to).await,
            None => recents::rename_path(from, to).await,
        }
    }

    async fn rename_path(&self, from: PathBuf, to: PathBuf) -> Result<PathBuf, BridgeError> {
        tokio::fs::rename(&from, &to)
            .await
            .map_err(|error| BridgeError::Handler {
                code: "rename_failed".to_string(),
                message: format!(
                    "failed to rename {} to {}: {error}",
                    from.display(),
                    to.display()
                ),
            })?;

        let renamed_current = {
            let mut state = self.state.lock().await;
            if state.current_path.as_deref() == Some(from.as_path()) {
                state.current_path = Some(to.clone());
                true
            } else {
                false
            }
        };
        if renamed_current {
            // TODO: update the native window title when DaemonWindowOps exposes
            // a verified title-update hook for the current notebook window.
            if let Err(error) = self.persist_last_notebook(&to).await {
                warn!(%error, path = %to.display(), "failed to persist renamed notebook record");
            }
        }

        self.rename_recent_path(&from, &to)
            .await
            .map_err(|error| BridgeError::Handler {
                code: "recents_failed".to_string(),
                message: error.to_string(),
            })?;
        self.windows.emit_recents_changed();
        Ok(to)
    }

    async fn open_path(&self, path: PathBuf) -> Result<PathBuf, BridgeError> {
        let (previous_path, previous_window_label) = {
            let state = self.state.lock().await;
            let previous_path = state.current_path.clone();
            let previous_window_label = state.window_label.clone();
            if state.current_path.as_deref() == Some(path.as_path()) {
                if let Some(label) = previous_window_label.clone() {
                    drop(state);
                    if self.windows.show_and_focus(&label) {
                        return Ok(path);
                    }
                }
            }

            (previous_path, previous_window_label)
        };

        // Try to open the new window first; only mutate state on success so a
        // failure leaves the previously open notebook recoverable (H2).
        let label = match self.windows.open_notebook_path(&path) {
            Ok(label) => label,
            Err(error) => {
                if previous_path.is_some() {
                    if let Some(label) = previous_window_label.as_deref() {
                        let _ = self.windows.show_and_focus(label);
                    }
                }
                return Err(error);
            }
        };

        if let Some(label) = previous_window_label.as_deref() {
            self.windows.hide(label);
        }
        self.bridge.set_notebook_open(false);
        {
            let mut state = self.state.lock().await;
            state.current_path = Some(path.clone());
            state.window_label = Some(label);
        }
        self.load_open_notebook(&path).await?;
        if let Err(error) = self.persist_last_notebook(&path).await {
            warn!(%error, path = %path.display(), "failed to persist last notebook record");
        }
        Ok(path)
    }

    async fn load_open_notebook(&self, path: &Path) -> Result<(), BridgeError> {
        self.wait_for_notebook_store(FLUSH_PENDING_TIMEOUT).await?;
        self.requester
            .request(
                "notebook.load",
                json!({ "path": path }),
                FLUSH_PENDING_TIMEOUT,
            )
            .await
            .map(|_| ())
    }

    async fn wait_for_notebook_store(&self, timeout: Duration) -> Result<(), BridgeError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if !self.requester.window_alive() {
                return Err(BridgeError::WindowClosed);
            }
            if self.requester.listener_registered() && self.requester.notebook_open() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                if !self.requester.notebook_open() {
                    return Err(BridgeError::NotebookNotOpen);
                }
                if !self.requester.listener_registered() {
                    return Err(BridgeError::NoListener);
                }
                return Err(BridgeError::Timeout);
            }
            tokio::time::sleep(NOTEBOOK_LOAD_READY_POLL).await;
        }
    }

    pub async fn reopen(&self) -> Result<PathBuf, BridgeError> {
        let (path, label) = {
            let state = self.state.lock().await;
            let path = state
                .current_path
                .clone()
                .ok_or(BridgeError::NotebookNotOpen)?;
            (path, state.window_label.clone())
        };

        if let Some(label) = label {
            if self.windows.show_and_focus(&label) {
                return Ok(path);
            }
        }

        self.open_path(path).await
    }

    async fn close_current_window(&self) {
        let label = self.state.lock().await.window_label.clone();
        if let Some(label) = label {
            self.windows.hide(&label);
        }
    }

    async fn persist_last_notebook(&self, path: &Path) -> Result<()> {
        match self.last_record_path.as_deref() {
            Some(record_path) => persist_last_notebook_at(record_path, path).await,
            None => persist_last_notebook(path).await,
        }
    }

    async fn clear_last_notebook(&self) -> Result<()> {
        match self.last_record_path.as_deref() {
            Some(record_path) => clear_last_notebook_at(record_path).await,
            None => clear_last_notebook().await,
        }
    }

    pub async fn restore_last_open_notebook(&self) {
        let Some(path) = notebook_path_for_daemon_start().await else {
            return;
        };
        if let Err(error) = self.open_path(path.clone()).await {
            warn!(
                %error,
                path = %path.display(),
                "failed to open notebook on daemon start"
            );
        }
    }
}

fn should_continue_without_flush(error: &BridgeError) -> bool {
    match error {
        BridgeError::NotebookNotOpen
        | BridgeError::WindowClosed
        | BridgeError::AppRestarted
        | BridgeError::NoListener
        | BridgeError::Timeout => true,
        BridgeError::Handler { code, .. } => code == "notebook_not_open",
    }
}

fn is_notebook_store_command(command: &str) -> bool {
    matches!(
        command,
        "write_cell"
            | "read_cell"
            | "insert_cell"
            | "delete_cell"
            | "snapshot"
            | "apply_edit"
            | "load"
            | "flush_notebook"
    )
}

fn notebook_store_request_from_daemon(
    request: DaemonControlRequest,
) -> std::result::Result<jute::commands::DaemonControlRequest, DaemonControlError> {
    let command = match request.command.as_str() {
        "write_cell" => jute::commands::DaemonControlCommand::WriteCell {
            id: required_cell_id(&request)?,
            source: required_string(&request.source, "write_cell requires source")?,
            expected_version: request.expected_version,
            last_edited_by: request.last_edited_by,
        },
        "read_cell" => jute::commands::DaemonControlCommand::ReadCell {
            id: required_cell_id(&request)?,
        },
        "insert_cell" => jute::commands::DaemonControlCommand::InsertCell {
            kind: request
                .kind
                .ok_or_else(|| invalid_params("insert_cell requires kind"))?,
            after_id: request.after_id,
            source: required_string(&request.source, "insert_cell requires source")?,
            last_edited_by: request.last_edited_by,
        },
        "load" => {
            let path = request
                .path
                .ok_or_else(|| invalid_params("load requires path"))?;
            jute::commands::DaemonControlCommand::LoadNotebook {
                path: path.to_string_lossy().into_owned(),
            }
        }
        "delete_cell" => jute::commands::DaemonControlCommand::DeleteCell {
            id: required_cell_id(&request)?,
            expected_version: request
                .expected_version
                .ok_or_else(|| invalid_params("delete_cell requires expected_version"))?,
        },
        "snapshot" => jute::commands::DaemonControlCommand::Snapshot {},
        "apply_edit" => jute::commands::DaemonControlCommand::ApplyEdit {
            id: required_cell_id(&request)?,
            source: required_string(&request.source, "apply_edit requires source")?,
        },
        "flush_notebook" => jute::commands::DaemonControlCommand::FlushNotebook {},
        command => {
            return Err(DaemonControlError {
                code: "unsupported_daemon_command".to_string(),
                message: format!("unsupported notebook store command: {command}"),
            })
        }
    };
    Ok(jute::commands::DaemonControlRequest::new(command))
}

fn required_cell_id(
    request: &DaemonControlRequest,
) -> std::result::Result<String, DaemonControlError> {
    required_string(&request.id, "cell command requires id")
}

fn required_string(
    value: &Option<String>,
    message: &str,
) -> std::result::Result<String, DaemonControlError> {
    value
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| invalid_params(message))
}

fn invalid_params(message: &str) -> DaemonControlError {
    DaemonControlError {
        code: "invalid_params".to_string(),
        message: message.to_string(),
    }
}

impl DaemonControlHandler for NotebookDaemonControl {
    fn handle_control<'a>(&'a self, request: DaemonControlRequest) -> DaemonControlFuture<'a> {
        Box::pin(async move { self.handle(request).await })
    }
}

const UNTITLED_NOTEBOOK_MAX_SUFFIX: usize = 1000;

async fn notebook_path_for_daemon_start() -> Option<PathBuf> {
    notebook_path_for_daemon_start_with(None, None).await
}

#[cfg(test)]
async fn notebook_path_for_daemon_start_at(
    record_path: &Path,
    untitled_dir: &Path,
) -> Option<PathBuf> {
    notebook_path_for_daemon_start_with(Some(record_path), Some(untitled_dir)).await
}

async fn notebook_path_for_daemon_start_with(
    record_path: Option<&Path>,
    untitled_dir: Option<&Path>,
) -> Option<PathBuf> {
    let target = match load_last_notebook_from(record_path).await {
        Ok(Some(path)) if path.exists() => Some(path),
        Ok(Some(stale)) => {
            warn!(
                path = %stale.display(),
                "last notebook no longer exists; clearing record"
            );
            if let Err(error) = clear_last_notebook_from(record_path).await {
                warn!(%error, "failed to clear stale last notebook record");
            }
            None
        }
        Ok(None) => None,
        Err(error) => {
            warn!(%error, "failed to load last notebook record");
            None
        }
    };

    match target {
        Some(path) => Some(path),
        None => match create_untitled_notebook_from(untitled_dir).await {
            Ok(path) => Some(path),
            Err(error) => {
                warn!(%error, "failed to create fallback Untitled notebook");
                None
            }
        },
    }
}

async fn load_last_notebook_from(record_path: Option<&Path>) -> Result<Option<PathBuf>> {
    match record_path {
        Some(record_path) => load_last_notebook_at(record_path).await,
        None => load_last_notebook().await,
    }
}

async fn clear_last_notebook_from(record_path: Option<&Path>) -> Result<()> {
    match record_path {
        Some(record_path) => clear_last_notebook_at(record_path).await,
        None => clear_last_notebook().await,
    }
}

async fn create_untitled_notebook_from(untitled_dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    match untitled_dir {
        Some(untitled_dir) => create_untitled_notebook_in_dir(untitled_dir).await,
        None => create_untitled_notebook().await,
    }
}

async fn create_untitled_notebook() -> anyhow::Result<PathBuf> {
    create_untitled_notebook_in_dir(&scratch_dir()?).await
}

async fn create_untitled_notebook_in_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let contents = serde_json::to_vec_pretty(&json!({
        "cells": [],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    }))?;

    for suffix in 0..=UNTITLED_NOTEBOOK_MAX_SUFFIX {
        let file_name = if suffix == 0 {
            "Untitled.ipynb".to_string()
        } else {
            format!("Untitled{suffix}.ipynb")
        };
        let path = dir.join(file_name);
        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", path.display()))
            }
        };
        file.write_all(&contents)
            .await
            .with_context(|| format!("failed to write {}", path.display()))?;
        return Ok(path);
    }

    anyhow::bail!(
        "failed to create an Untitled notebook in {} after trying suffixes 0 through {}",
        dir.display(),
        UNTITLED_NOTEBOOK_MAX_SUFFIX
    );
}

pub async fn start_daemon_server(
    socket_path: impl AsRef<Path>,
    lifecycle_bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
    state: Arc<State>,
) -> Result<(NotebookMcpServerHandle, NotebookDaemonControl)> {
    start_daemon_server_with_config(
        socket_path,
        lifecycle_bridge,
        app,
        state,
        NotebookConfig::default(),
    )
    .await
}

/// Starts the daemon with a bridge for window-lifecycle signals.
/// The lifecycle bridge is always used for notebook-open and shutdown-drain plumbing.
/// When `in_proc_store` is false, it is also wrapped as the MCP transport.
pub async fn start_daemon_server_with_config(
    socket_path: impl AsRef<Path>,
    lifecycle_bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
    state: Arc<State>,
    config: NotebookConfig,
) -> Result<(NotebookMcpServerHandle, NotebookDaemonControl)> {
    let socket_path = socket_path.as_ref().to_path_buf();
    let app_for_deps = app.clone();
    let requester: Arc<dyn BridgeRequester> = if config.in_proc_store {
        Arc::new(LoopbackDaemonRequester::new(socket_path.clone()))
    } else {
        Arc::new(TauriBridgeRequester::with_app(
            Arc::clone(&lifecycle_bridge),
            app.clone(),
        ))
    };
    let windows: Arc<dyn DaemonWindowOps> = Arc::new(TauriDaemonWindowOps { app });
    let control = NotebookDaemonControl::new_with_parts(
        lifecycle_bridge,
        Arc::clone(&requester),
        Arc::clone(&state),
        windows,
        None,
    );
    let deps = Arc::new(ServerDeps {
        bridge: requester,
        state: Some(state),
        app: Some(app_for_deps),
        daemon: Some(control.clone()),
    });
    let handle = start_multiplexed_server(socket_path, deps, control.clone()).await?;
    control.restore_last_open_notebook().await;
    Ok((handle, control))
}

async fn start_multiplexed_server(
    socket_path: impl AsRef<Path>,
    deps: Arc<ServerDeps>,
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
                    let deps = Arc::clone(&deps);
                    let control = control.clone();
                    tokio::spawn(async move {
                        handle_daemon_connection(stream, deps, control).await;
                    });
                }
            }
        }
        deps.bridge.drain_on_shutdown().await;
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
    deps: Arc<ServerDeps>,
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
                entries: None,
                result: None,
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
    match NotebookMcpServer::new(deps).serve(transport).await {
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use tokio::net::UnixStream;
    use tokio::time::{timeout, Duration};

    use jute::state::State;

    use super::bridge::BridgeRequestFuture;

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
                    entries: None,
                    result: None,
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

    fn test_server_deps() -> Arc<ServerDeps> {
        Arc::new(ServerDeps::from_bridge(test_bridge_requester()))
    }

    struct BufferedNotebookBridge {
        notebook: tokio::sync::Mutex<Value>,
        path: PathBuf,
        calls: tokio::sync::Mutex<Vec<String>>,
    }

    impl BufferedNotebookBridge {
        fn new(path: PathBuf, notebook: Value) -> Self {
            Self {
                notebook: tokio::sync::Mutex::new(notebook),
                path,
                calls: tokio::sync::Mutex::new(Vec::new()),
            }
        }

        async fn calls(&self) -> Vec<String> {
            self.calls.lock().await.clone()
        }
    }

    impl BridgeRequester for BufferedNotebookBridge {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            method: &'static str,
            params: Value,
            _timeout: Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async move {
                self.calls.lock().await.push(method.to_string());
                match method {
                    "notebook.insert_cell" => {
                        let kind = params["kind"]
                            .as_str()
                            .expect("insert_cell kind")
                            .to_string();
                        let source = params["source"]
                            .as_str()
                            .expect("insert_cell source")
                            .to_string();
                        let last_edited_by = params["last_edited_by"]
                            .as_str()
                            .unwrap_or("brain")
                            .to_string();
                        let is_code = kind == "code";
                        let id = "inserted-cell".to_string();
                        let mut cell = json!({
                            "cell_type": kind,
                            "id": id,
                            "metadata": {
                                "spur": {
                                    "version": 1,
                                    "last_edited_by": last_edited_by
                                }
                            },
                            "source": source
                        });
                        if is_code {
                            cell["execution_count"] = Value::Null;
                            cell["outputs"] = json!([]);
                        }

                        let mut notebook = self.notebook.lock().await;
                        notebook["cells"]
                            .as_array_mut()
                            .expect("notebook cells")
                            .push(cell);
                        Ok(json!({ "id": id, "version": 1 }))
                    }
                    "notebook.load" => Ok(Value::Null),
                    _ => Err(BridgeError::Handler {
                        code: "unknown_method".to_string(),
                        message: format!("unexpected method: {method}"),
                    }),
                }
            })
        }

        fn flush_pending<'a>(&'a self, _timeout: Duration) -> BridgeRequestFuture<'a> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .await
                    .push("notebook.flush_pending".to_string());
                Ok(json!({
                    "path": self.path.display().to_string(),
                    "contents": self.notebook.lock().await.clone()
                }))
            })
        }
    }

    #[derive(Default)]
    struct RecordingWindowOps {
        opened: StdMutex<Vec<PathBuf>>,
        hidden: StdMutex<Vec<String>>,
        recents_changed: AtomicUsize,
        exited: AtomicBool,
    }

    impl RecordingWindowOps {
        fn opened(&self) -> Vec<PathBuf> {
            self.opened.lock().expect("opened lock").clone()
        }

        fn hidden(&self) -> Vec<String> {
            self.hidden.lock().expect("hidden lock").clone()
        }

        fn recents_changed_count(&self) -> usize {
            self.recents_changed.load(Ordering::SeqCst)
        }
    }

    impl DaemonWindowOps for RecordingWindowOps {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, label: &str) {
            self.hidden
                .lock()
                .expect("hidden lock")
                .push(label.to_string());
        }

        fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
            self.opened
                .lock()
                .expect("opened lock")
                .push(path.to_path_buf());
            Ok(format!("window-{}", self.opened().len()))
        }

        fn emit_recents_changed(&self) {
            self.recents_changed.fetch_add(1, Ordering::SeqCst);
        }

        fn exit(&self) {
            self.exited.store(true, Ordering::SeqCst);
        }
    }

    fn empty_notebook() -> Value {
        json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": []
        })
    }

    #[tokio::test]
    async fn open_flushes_buffered_edits_to_current_path_before_switching() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-daemon-flush-")
            .tempdir()
            .expect("temp dir");
        let current_path = temp_dir.path().join("current.ipynb");
        let other_path = temp_dir.path().join("other.ipynb");
        let last_record_path = temp_dir.path().join("last.json");
        let recents_record_path = temp_dir.path().join("recents.json");
        tokio::fs::write(
            &current_path,
            serde_json::to_vec_pretty(&empty_notebook()).unwrap(),
        )
        .await
        .expect("current notebook writes");

        let buffered_bridge = Arc::new(BufferedNotebookBridge::new(
            current_path.clone(),
            empty_notebook(),
        ));
        let requester: Arc<dyn BridgeRequester> = buffered_bridge.clone();
        let notebook_state = Arc::new(State::new());
        let windows = Arc::new(RecordingWindowOps::default());
        let window_ops: Arc<dyn DaemonWindowOps> = windows.clone();
        let control = NotebookDaemonControl::new_for_test_with_recents_record(
            Arc::new(AgentBridge::new()),
            requester.clone(),
            notebook_state,
            window_ops,
            Some(last_record_path),
            Some(recents_record_path),
        );
        {
            let mut state = control.state.lock().await;
            state.current_path = Some(current_path.clone());
            state.window_label = Some("current-window".to_string());
        }

        let deps = ServerDeps::from_bridge(requester);
        tools::insert_cell::call(
            &deps,
            json!({
                "kind": "markdown",
                "source": "persist me before switching"
            }),
        )
        .await
        .expect("insert_cell mutates in-memory notebook");

        let response = control
            .handle(DaemonControlRequest {
                id: None,
                daemon: None,
                command: "open".to_string(),
                path: Some(other_path.clone()),
                pinned: None,
                ..Default::default()
            })
            .await;

        assert!(response.ok, "{:?}", response.error);
        assert_eq!(response.path, Some(other_path.display().to_string()));
        assert_eq!(windows.hidden(), vec!["current-window"]);
        assert_eq!(windows.opened(), vec![other_path]);
        assert_eq!(
            buffered_bridge.calls().await,
            vec![
                "notebook.insert_cell",
                "notebook.flush_pending",
                "notebook.load"
            ]
        );

        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(&current_path)
                .await
                .expect("current notebook reads"),
        )
        .expect("saved notebook parses");
        assert_eq!(saved["cells"][0]["source"], "persist me before switching");
        assert_eq!(
            saved["cells"][0]["metadata"]["spur"]["last_edited_by"],
            "brain"
        );
    }

    #[tokio::test]
    async fn rename_moves_file_updates_current_path_and_recents() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-daemon-rename-")
            .tempdir()
            .expect("temp dir");
        let from = temp_dir.path().join("old-name.ipynb");
        let to = temp_dir.path().join("new-name.ipynb");
        let last_record_path = temp_dir.path().join("last.json");
        let recents_record_path = temp_dir.path().join("recents.json");
        tokio::fs::write(&from, serde_json::to_vec_pretty(&empty_notebook()).unwrap())
            .await
            .expect("source notebook writes");
        tokio::fs::write(
            &recents_record_path,
            serde_json::to_vec_pretty(&json!({
                "entries": [{
                    "path": from,
                    "lastOpened": chrono::Utc::now(),
                    "isScratch": false,
                    "pinned": true
                }]
            }))
            .unwrap(),
        )
        .await
        .expect("recents writes");

        let windows = Arc::new(RecordingWindowOps::default());
        let window_ops: Arc<dyn DaemonWindowOps> = windows.clone();
        let control = NotebookDaemonControl::new_for_test_with_recents_record(
            Arc::new(AgentBridge::new()),
            test_bridge_requester(),
            Arc::new(State::new()),
            window_ops,
            Some(last_record_path.clone()),
            Some(recents_record_path.clone()),
        );
        {
            let mut state = control.state.lock().await;
            state.current_path = Some(from.clone());
            state.window_label = Some("current-window".to_string());
        }

        let response = control
            .handle(DaemonControlRequest {
                id: None,
                daemon: None,
                command: "rename".to_string(),
                from: Some(from.clone()),
                to: Some(to.clone()),
                ..Default::default()
            })
            .await;

        assert!(response.ok, "{:?}", response.error);
        assert_eq!(response.path, Some(to.display().to_string()));
        assert!(!from.exists());
        assert!(to.exists());

        let state = control.state.lock().await;
        assert_eq!(state.current_path.as_deref(), Some(to.as_path()));
        drop(state);
        assert_eq!(
            load_last_notebook_at(&last_record_path).await.unwrap(),
            Some(to.clone())
        );

        let recents: Value =
            serde_json::from_slice(&tokio::fs::read(&recents_record_path).await.unwrap()).unwrap();
        assert_eq!(recents["entries"][0]["path"], to.display().to_string());
        assert_eq!(recents["entries"][0]["pinned"], true);
        assert_eq!(windows.recents_changed_count(), 1);
    }
    // DaemonWindowOps keeps lifecycle tests independent of a concrete Tauri
    // AppHandle; the production implementation still delegates to Tauri.

    fn spawn_multiplexer(control: RecordingDaemonControl) -> (UnixStream, JoinHandle<()>) {
        let (server, client) = UnixStream::pair().expect("in-memory stream pair");
        let handler = tokio::spawn(async move {
            handle_daemon_connection(server, test_server_deps(), control).await;
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

    #[test]
    fn server_tool_inventory_includes_venv_tools() {
        let server = NotebookMcpServer::new(test_server_deps());
        let names = server
            .tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();

        for expected in [
            "notebook.venv_list",
            "notebook.venv_create",
            "notebook.venv_delete",
            "notebook.venv_list_python_versions",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing tool: {expected}"
            );
        }
    }

    #[derive(Default)]
    struct FailingWindowOps;

    impl DaemonWindowOps for FailingWindowOps {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, _label: &str) {}

        fn open_notebook_path(&self, _path: &Path) -> Result<String, BridgeError> {
            Err(BridgeError::Handler {
                code: "window_open_failed".to_string(),
                message: "mock jute failure".to_string(),
            })
        }

        fn emit_recents_changed(&self) {}

        fn exit(&self) {}
    }

    #[tokio::test]
    async fn open_path_failure_preserves_previous_window_state() {
        let bridge = Arc::new(AgentBridge::new());
        bridge.set_notebook_open(true);
        let requester: Arc<dyn BridgeRequester> = test_bridge_requester();
        let windows: Arc<dyn DaemonWindowOps> = Arc::new(FailingWindowOps);
        let control = NotebookDaemonControl::new_for_test(
            Arc::clone(&bridge),
            requester,
            Arc::new(State::new()),
            windows,
            None,
        );
        let previous_path = PathBuf::from("/tmp/previous.ipynb");
        let previous_label = "jute-window-existing".to_string();

        {
            let mut state = control.state.lock().await;
            state.current_path = Some(previous_path.clone());
            state.window_label = Some(previous_label.clone());
        }

        let error = control
            .open_path(PathBuf::from("/tmp/new.ipynb"))
            .await
            .expect_err("window open failure should bubble");

        assert!(matches!(
            error,
            BridgeError::Handler { ref code, .. } if code == "window_open_failed"
        ));

        let state = control.state.lock().await;
        assert_eq!(state.current_path.as_deref(), Some(previous_path.as_path()));
        assert_eq!(state.window_label.as_deref(), Some(previous_label.as_str()));
        assert!(bridge.notebook_open());
    }

    #[tokio::test]
    async fn venv_tool_dispatch_reaches_app_handle_requirement() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-venv-mcp-")
            .tempdir()
            .expect("temp dir");
        let socket_path = temp_dir.path().join("notebook.sock");
        let _server = start_server(&socket_path).await.expect("server starts");
        let stream = UnixStream::connect(&socket_path)
            .await
            .expect("client connects");
        let transport = LengthPrefixedJsonTransport::new(stream);
        let client = rmcp::model::ClientInfo::default()
            .serve(transport)
            .await
            .expect("client initializes");

        let error = client
            .call_tool(CallToolRequestParams::new("notebook.venv_list"))
            .await
            .expect_err("tool routes but cannot run without app handle");
        let message = error.to_string();
        assert!(
            message.contains("Tauri app handle"),
            "unexpected error: {message}"
        );

        client.cancel().await.expect("client closes");
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
            .tempdir()
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

    #[tokio::test]
    async fn create_untitled_notebook_uses_jupyter_style_names_and_fills_gaps() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-untitled-")
            .tempdir()
            .expect("temp dir");

        let first = create_untitled_notebook_in_dir(temp_dir.path())
            .await
            .expect("first untitled notebook writes");
        let second = create_untitled_notebook_in_dir(temp_dir.path())
            .await
            .expect("second untitled notebook writes");
        let third = create_untitled_notebook_in_dir(temp_dir.path())
            .await
            .expect("third untitled notebook writes");

        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("Untitled.ipynb")
        );
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("Untitled1.ipynb")
        );
        assert_eq!(
            third.file_name().and_then(|name| name.to_str()),
            Some("Untitled2.ipynb")
        );

        tokio::fs::remove_file(&first)
            .await
            .expect("remove first untitled notebook");

        let fills_gap = create_untitled_notebook_in_dir(temp_dir.path())
            .await
            .expect("gap-filling untitled notebook writes");
        assert_eq!(
            fills_gap.file_name().and_then(|name| name.to_str()),
            Some("Untitled.ipynb")
        );
    }

    #[tokio::test]
    async fn daemon_start_with_stale_last_record_clears_and_creates_untitled() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-start-stale-")
            .tempdir()
            .expect("temp dir");
        let record_path = temp_dir.path().join("last.json");
        let stale_path = temp_dir.path().join("missing.ipynb");

        persist_last_notebook_at(&record_path, &stale_path)
            .await
            .expect("stale record writes");

        let path = notebook_path_for_daemon_start_at(&record_path, temp_dir.path())
            .await
            .expect("fallback notebook path");

        assert_eq!(path, temp_dir.path().join("Untitled.ipynb"));
        assert!(path.exists());
        assert_eq!(
            load_last_notebook_at(&record_path)
                .await
                .expect("cleared record reads as none"),
            None
        );
    }

    #[tokio::test]
    async fn daemon_start_without_last_record_creates_untitled() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-start-absent-")
            .tempdir()
            .expect("temp dir");
        let record_path = temp_dir.path().join("last.json");

        let path = notebook_path_for_daemon_start_at(&record_path, temp_dir.path())
            .await
            .expect("fallback notebook path");

        assert_eq!(path, temp_dir.path().join("Untitled.ipynb"));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn daemon_start_with_unreadable_last_record_creates_untitled() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-start-unreadable-")
            .tempdir()
            .expect("temp dir");
        let record_path = temp_dir.path().join("last.json");
        tokio::fs::write(&record_path, b"{not-json")
            .await
            .expect("unreadable record writes");

        let path = notebook_path_for_daemon_start_at(&record_path, temp_dir.path())
            .await
            .expect("fallback notebook path");

        assert_eq!(path, temp_dir.path().join("Untitled.ipynb"));
        assert!(path.exists());
    }
}
