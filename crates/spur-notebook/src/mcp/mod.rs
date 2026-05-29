use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use jute::{
    backend::notebook::NotebookRoot,
    commands::{kernel_slot_info_for_state, RecentNotebookEntry, RecentsChangedEvent},
    state::{notebook_slot_id, State},
};
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
    fn emit_recents_changed(&self, event: &RecentsChangedEvent);
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

    fn emit_recents_changed(&self, event: &RecentsChangedEvent) {
        let _ = self.app.emit(self::tools::RECENTS_CHANGED_EVENT, event);
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

#[derive(Debug)]
pub struct DaemonControlRequest {
    pub id: Option<String>,
    pub request: jute::commands::DaemonControlRequest,
}

impl<'de> Deserialize<'de> for DaemonControlRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .cloned()
            .map(String::deserialize)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        let request = jute::commands::DaemonControlRequest::deserialize(value)
            .map_err(serde::de::Error::custom)?;

        Ok(Self { id, request })
    }
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
    result: Option<Value>,
}

impl DaemonControlSuccess {
    fn empty() -> Self {
        Self {
            path: None,
            entries: None,
            result: None,
        }
    }

    fn path(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            entries: None,
            result: None,
        }
    }

    fn entries(entries: Vec<RecentEntry>) -> Self {
        Self {
            path: None,
            entries: Some(entries),
            result: None,
        }
    }

    #[cfg(feature = "datasource-introspect")]
    fn result(result: Value) -> Self {
        Self {
            path: None,
            entries: None,
            result: Some(result),
        }
    }
}

fn recents_bridge_error(error: anyhow::Error) -> BridgeError {
    BridgeError::Handler {
        code: "recents_failed".to_string(),
        message: error.to_string(),
    }
}

#[cfg(feature = "datasource-introspect")]
fn normalize_datasource_path(path: String) -> Result<PathBuf, BridgeError> {
    if path.trim().is_empty() {
        return Err(BridgeError::Handler {
            code: "invalid_datasource_path".to_string(),
            message: "datasource path must not be empty".to_string(),
        });
    }

    std::fs::canonicalize(&path).map_err(|error| BridgeError::Handler {
        code: "invalid_datasource_path".to_string(),
        message: format!("failed to resolve datasource path {path}: {error}"),
    })
}

#[cfg(feature = "datasource-introspect")]
fn infer_datasource_kind(path: &Path) -> Result<jute::commands::DatasourceKind, BridgeError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);

    match extension.as_deref() {
        Some("csv") => Ok(jute::commands::DatasourceKind::Csv),
        Some("parquet" | "parq") => Ok(jute::commands::DatasourceKind::Parquet),
        Some("json" | "jsonl" | "ndjson") => Ok(jute::commands::DatasourceKind::Json),
        _ => Err(BridgeError::Handler {
            code: "unsupported_datasource_kind".to_string(),
            message: format!(
                "unsupported datasource file extension for {}",
                path.display()
            ),
        }),
    }
}

impl NotebookDaemonControl {
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

    pub async fn handle(&self, envelope: DaemonControlRequest) -> DaemonControlResponse {
        use jute::commands::DaemonControlCommand;

        let DaemonControlRequest { id, request } = envelope;
        if request.daemon != "notebook.v1" {
            return DaemonControlResponse {
                id,
                ok: false,
                path: None,
                entries: None,
                result: None,
                error: Some(DaemonControlError {
                    code: "invalid_control_message".to_owned(),
                    message: format!("unsupported daemon discriminator: {}", request.daemon),
                }),
            };
        }

        let result: Result<DaemonControlSuccess, BridgeError> =
            match request.command.clone() {
                DaemonControlCommand::WriteCell { .. }
                | DaemonControlCommand::ReadCell { .. }
                | DaemonControlCommand::InsertCell { .. }
                | DaemonControlCommand::LoadNotebook { .. }
                | DaemonControlCommand::DeleteCell { .. }
                | DaemonControlCommand::Snapshot {}
                | DaemonControlCommand::ApplyEdit { .. }
                | DaemonControlCommand::FlushNotebook {} => {
                    return self.handle_notebook_store_control(id, request).await;
                }
                DaemonControlCommand::Open { path } => async {
                    self.save_current().await?;
                    let path = self
                        .open_path(resolve_notebook_path(PathBuf::from(path)))
                        .await?;
                    if let Err(error) = self.record_recent_open(&path).await {
                        warn!(%error, path = %path.display(), "failed to record recent notebook");
                    } else {
                        self.emit_recents_changed().await?;
                    }
                    Ok(DaemonControlSuccess::path(path))
                }
                .await,
                DaemonControlCommand::New {} => async {
                    self.save_current().await?;
                    let path =
                        create_untitled_notebook()
                            .await
                            .map_err(|error| BridgeError::Handler {
                                code: "scratch_create_failed".to_owned(),
                                message: error.to_string(),
                            })?;
                    let path = self.open_path(path).await?;
                    if let Err(error) = self.record_recent_open(&path).await {
                        warn!(%error, path = %path.display(), "failed to record recent notebook");
                    } else {
                        self.emit_recents_changed().await?;
                    }
                    Ok(DaemonControlSuccess::path(path))
                }
                .await,
                DaemonControlCommand::NewAt { path } => async {
                    self.save_current().await?;
                    let path = resolve_notebook_path(PathBuf::from(path));
                    create_empty_notebook_at(&path).await.map_err(|error| {
                        BridgeError::Handler {
                            code: "notebook_create_failed".to_owned(),
                            message: error.to_string(),
                        }
                    })?;
                    let path = self.open_path(path).await?;
                    if let Err(error) = self.record_recent_open(&path).await {
                        warn!(%error, path = %path.display(), "failed to record recent notebook");
                    } else {
                        self.emit_recents_changed().await?;
                    }
                    Ok(DaemonControlSuccess::path(path))
                }
                .await,
                DaemonControlCommand::Reopen {} => {
                    self.reopen().await.map(DaemonControlSuccess::path)
                }
                DaemonControlCommand::Rename { from, to } => {
                    async {
                        self.save_current().await?;
                        let path = self
                            .rename_path(
                                resolve_notebook_path(PathBuf::from(from)),
                                resolve_notebook_path(PathBuf::from(to)),
                            )
                            .await?;
                        Ok(DaemonControlSuccess::path(path))
                    }
                    .await
                }
                DaemonControlCommand::Close {} => {
                    async {
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
                        self.emit_recents_changed().await?;
                        Ok(DaemonControlSuccess::empty())
                    }
                    .await
                }
                DaemonControlCommand::AttachDatasource { name, path, group } => {
                    self.attach_datasource(name, path, group).await
                }
                DaemonControlCommand::ListRecents {} => self
                    .list_recent_entries()
                    .await
                    .map(DaemonControlSuccess::entries)
                    .map_err(|error| BridgeError::Handler {
                        code: "recents_failed".to_owned(),
                        message: error.to_string(),
                    }),
                DaemonControlCommand::RemoveFromRecents { path } => {
                    async {
                        self.remove_recent_path(&resolve_notebook_path(PathBuf::from(path)))
                            .await
                            .map_err(|error| BridgeError::Handler {
                                code: "recents_failed".to_owned(),
                                message: error.to_string(),
                            })?;
                        self.emit_recents_changed().await?;
                        Ok(DaemonControlSuccess::empty())
                    }
                    .await
                }
                DaemonControlCommand::SetPinned { path, pinned } => {
                    async {
                        self.set_recent_pinned(&resolve_notebook_path(PathBuf::from(path)), pinned)
                            .await
                            .map_err(|error| BridgeError::Handler {
                                code: "recents_failed".to_owned(),
                                message: error.to_string(),
                            })?;
                        self.emit_recents_changed().await?;
                        Ok(DaemonControlSuccess::empty())
                    }
                    .await
                }
            };

        match result {
            Ok(success) => DaemonControlResponse {
                id,
                ok: true,
                path: success.path.map(|path| path.display().to_string()),
                entries: success.entries,
                result: success.result,
                error: None,
            },
            Err(error) => DaemonControlResponse {
                id,
                ok: false,
                path: None,
                entries: None,
                result: None,
                error: Some(DaemonControlError {
                    code: error.mcp_code().to_owned(),
                    message: error.to_string(),
                }),
            },
        }
    }

    #[cfg(feature = "datasource-introspect")]
    async fn attach_datasource(
        &self,
        name: String,
        path: String,
        group: Option<String>,
    ) -> Result<DaemonControlSuccess, BridgeError> {
        let path = normalize_datasource_path(path)?;
        let kind = infer_datasource_kind(&path)?;
        let schema = crate::datasource::introspect_datasource(&path, kind).map_err(|error| {
            BridgeError::Handler {
                code: "datasource_introspection_failed".to_string(),
                message: error.to_string(),
            }
        })?;

        let entry = jute::commands::DatasourceEntry {
            name,
            path: path.display().to_string(),
            kind,
            group,
            columns: schema.columns,
            row_count: schema.row_count,
        };

        self.jute_state.attach_datasource(entry.clone());
        self.persist_catalog_to_current_notebook().await?;

        let result = serde_json::to_value(entry).map_err(|error| BridgeError::Handler {
            code: "datasource_entry_encode_failed".to_string(),
            message: error.to_string(),
        })?;

        Ok(DaemonControlSuccess::result(result))
    }

    #[cfg(not(feature = "datasource-introspect"))]
    async fn attach_datasource(
        &self,
        _name: String,
        _path: String,
        _group: Option<String>,
    ) -> Result<DaemonControlSuccess, BridgeError> {
        Err(BridgeError::Handler {
            code: "datasource_introspect_unavailable".to_string(),
            message: "datasource introspection is disabled".to_string(),
        })
    }

    pub async fn current_path(&self) -> Option<PathBuf> {
        self.state.lock().await.current_path.clone()
    }

    async fn handle_notebook_store_control(
        &self,
        id: Option<String>,
        request: jute::commands::DaemonControlRequest,
    ) -> DaemonControlResponse {
        let response =
            jute::commands::handle_daemon_control_request(request, &self.jute_state).await;
        DaemonControlResponse {
            id,
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

    #[cfg(feature = "datasource-introspect")]
    async fn persist_catalog_to_current_notebook(&self) -> Result<(), BridgeError> {
        let path = {
            let state = self.state.lock().await;
            state.current_path.clone()
        };
        let Some(path) = path else {
            return Ok(());
        };

        let (snapshot, _) = self.jute_state.get_notebook().snapshot();
        self.jute_state
            .save_coordinator
            .save(path, snapshot)
            .await
            .map_err(|error| BridgeError::Handler {
                code: "catalog_persist_failed".to_string(),
                message: error.to_string(),
            })
    }

    async fn record_recent_open(&self, path: &Path) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::record_open_at(record_path, &scratch_dir()?, path).await,
            None => recents::record_open(path).await,
        }
    }

    async fn list_recent_entries(&self) -> Result<Vec<RecentEntry>> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::list_recents_at(record_path).await,
            None => recents::list_recents().await,
        }
    }

    async fn remove_recent_path(&self, path: &Path) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::remove_from_recents_at(record_path, path).await,
            None => recents::remove_from_recents(path).await,
        }
    }

    async fn set_recent_pinned(&self, path: &Path, pinned: bool) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::set_pinned_at(record_path, path, pinned).await,
            None => recents::set_pinned(path, pinned).await,
        }
    }

    async fn rename_recent_path(&self, from: &Path, to: &Path) -> Result<()> {
        match self.recents_record_path.as_deref() {
            Some(record_path) => recents::rename_path_at(record_path, from, to).await,
            None => recents::rename_path(from, to).await,
        }
    }

    async fn recents_changed_event(&self) -> Result<RecentsChangedEvent, BridgeError> {
        let current_path = self.current_path_for_recents_event().await?;
        let entries = self
            .list_recent_entries()
            .await
            .map_err(recents_bridge_error)?;
        let mut event_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let normalized_path = recents::canonicalize_or_normalize(&entry.path)
                .await
                .map_err(recents_bridge_error)?;
            let path = normalized_path.display().to_string();
            event_entries.push(RecentNotebookEntry {
                path: path.clone(),
                last_opened: entry.last_opened.to_rfc3339(),
                is_scratch: entry.is_scratch,
                pinned: entry.pinned,
                kernel_alive: self.kernel_alive_for_notebook(&path).await,
                is_current: current_path.as_ref() == Some(&normalized_path),
            });
        }
        Ok(RecentsChangedEvent {
            entries: event_entries,
        })
    }

    async fn current_path_for_recents_event(&self) -> Result<Option<PathBuf>, BridgeError> {
        let current_path = {
            let state = self.state.lock().await;
            state.current_path.clone()
        };
        match current_path {
            Some(path) => recents::canonicalize_or_normalize(&path)
                .await
                .map(Some)
                .map_err(recents_bridge_error),
            None => Ok(None),
        }
    }

    async fn kernel_alive_for_notebook(&self, path: &str) -> bool {
        let slot_id = notebook_slot_id(path);
        kernel_slot_info_for_state(&slot_id, &self.jute_state)
            .await
            .map(|info| info.status != "dead")
            .unwrap_or(false)
    }

    async fn emit_recents_changed(&self) -> Result<(), BridgeError> {
        let event = self.recents_changed_event().await?;
        self.windows.emit_recents_changed(&event);
        Ok(())
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
        self.emit_recents_changed().await?;
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

fn empty_notebook_bytes() -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&json!({
        "cells": [],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    }))?)
}

pub(crate) async fn create_empty_notebook_at(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = empty_notebook_bytes()?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(&contents)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

async fn create_untitled_notebook_in_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let contents = empty_notebook_bytes()?;

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
    let socket_path = socket_path.as_ref().to_path_buf();
    let app_for_deps = app.clone();
    let requester: Arc<dyn BridgeRequester> =
        Arc::new(LoopbackDaemonRequester::new(socket_path.clone()));
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
    use std::sync::atomic::{AtomicBool, Ordering};
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
                let command = serde_json::to_value(&request.request.command)
                    .expect("daemon command serializes")
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("daemon command has tag")
                    .to_owned();
                self.commands.lock().expect("commands lock").push(command);
                DaemonControlResponse {
                    id: request.id,
                    ok: false,
                    path: None,
                    entries: None,
                    result: None,
                    error: Some(DaemonControlError {
                        code: "recorded_control_request".to_owned(),
                        message: "recorded control request".to_owned(),
                    }),
                }
            })
        }
    }

    #[test]
    fn daemon_control_request_decodes_flat_wire_as_typed_commands() {
        let request: DaemonControlRequest = serde_json::from_value(json!({
            "id": "request-1",
            "daemon": "notebook.v1",
            "command": "open",
            "path": "/tmp/notebook.ipynb"
        }))
        .expect("control daemon request decodes");

        assert_eq!(request.id.as_deref(), Some("request-1"));
        match request.request.command {
            jute::commands::DaemonControlCommand::Open { path } => {
                assert_eq!(path, "/tmp/notebook.ipynb");
            }
            command => panic!("unexpected command: {command:?}"),
        }

        let request: DaemonControlRequest = serde_json::from_value(json!({
            "daemon": "notebook.v1",
            "command": "write_cell",
            "id": "cell-1",
            "source": "print(1)",
            "expected_version": 7,
            "last_edited_by": "brain"
        }))
        .expect("daemon control request decodes");

        assert_eq!(request.id.as_deref(), Some("cell-1"));
        match request.request.command {
            jute::commands::DaemonControlCommand::WriteCell {
                id,
                source,
                expected_version,
                last_edited_by,
            } => {
                assert_eq!(id, "cell-1");
                assert_eq!(source, "print(1)");
                assert_eq!(expected_version, Some(7));
                assert_eq!(last_edited_by.as_deref(), Some("brain"));
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[cfg(feature = "datasource-introspect")]
    #[tokio::test]
    async fn attach_introspects_csv_schema() {
        let tempdir = tempfile::tempdir().expect("csv fixture dir");
        let csv = tempdir.path().join("sales.csv");
        std::fs::write(&csv, "region,revenue\nwest,10\neast,20\n").expect("write csv fixture");

        let jute_state = Arc::new(State::new());
        let control = NotebookDaemonControl::new_for_test(
            Arc::new(AgentBridge::new()),
            test_bridge_requester(),
            Arc::clone(&jute_state),
            Arc::new(RecordingWindowOps::default()),
            None,
        );

        let response = control
            .handle(daemon_request(
                jute::commands::DaemonControlCommand::AttachDatasource {
                    name: "sales".to_owned(),
                    path: csv.display().to_string(),
                    group: Some("quarterly".to_owned()),
                },
            ))
            .await;

        assert!(response.ok);
        let entry: jute::commands::DatasourceEntry =
            serde_json::from_value(response.result.expect("datasource entry result"))
                .expect("datasource entry decodes");

        assert_eq!(entry.name, "sales");
        assert_eq!(entry.path, csv.display().to_string());
        assert_eq!(entry.kind, jute::commands::DatasourceKind::Csv);
        assert_eq!(entry.group.as_deref(), Some("quarterly"));
        assert_eq!(
            entry.columns,
            vec![
                jute::commands::Column {
                    name: "region".to_owned(),
                    sql_type: "VARCHAR".to_owned(),
                },
                jute::commands::Column {
                    name: "revenue".to_owned(),
                    sql_type: "BIGINT".to_owned(),
                },
            ]
        );
        assert_eq!(entry.row_count, Some(2));
        assert_eq!(jute_state.datasource_catalog.lock().list(), vec![entry]);

        assert!(response.path.is_none());
        assert!(response.entries.is_none());
        assert!(response.error.is_none());
    }

    #[cfg(feature = "datasource-introspect")]
    #[tokio::test]
    async fn catalog_change_pushes_subscriber() {
        let tempdir = tempfile::tempdir().expect("csv fixture dir");
        let csv = tempdir.path().join("sales.csv");
        std::fs::write(&csv, "region,revenue\nwest,10\neast,20\n").expect("write csv fixture");

        let jute_state = Arc::new(State::new());
        let mut events = jute_state.event_tx.subscribe();
        let control = NotebookDaemonControl::new_for_test(
            Arc::new(AgentBridge::new()),
            test_bridge_requester(),
            Arc::clone(&jute_state),
            Arc::new(RecordingWindowOps::default()),
            None,
        );

        let response = control
            .handle(daemon_request(
                jute::commands::DaemonControlCommand::AttachDatasource {
                    name: "sales".to_owned(),
                    path: csv.display().to_string(),
                    group: Some("quarterly".to_owned()),
                },
            ))
            .await;

        assert!(response.ok, "{:?}", response.error);
        let entry: jute::commands::DatasourceEntry =
            serde_json::from_value(response.result.expect("datasource entry result"))
                .expect("datasource entry decodes");
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("catalog event is pushed")
            .expect("catalog event receiver stays open");

        assert_eq!(
            event,
            jute::state::DaemonEvent::DatasourcesChanged(vec![entry.clone()])
        );

        let removed = jute_state.detach_datasource("sales");
        assert_eq!(removed, Some(entry));
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("detach catalog event is pushed")
            .expect("catalog event receiver stays open");

        assert_eq!(
            event,
            jute::state::DaemonEvent::DatasourcesChanged(Vec::new())
        );
    }

    #[cfg(feature = "datasource-introspect")]
    #[tokio::test]
    async fn catalog_hydrates_from_metadata() {
        let workspace_dir = std::env::current_dir().expect("current dir");
        let tempdir = tempfile::Builder::new()
            .prefix("spur-notebook-catalog-hydrate-")
            .tempdir_in(&workspace_dir)
            .expect("workspace-local fixture dir");
        let csv = tempdir.path().join("sales.csv");
        let notebook_path = tempdir.path().join("analysis.ipynb");
        std::fs::write(&csv, "region,revenue\nwest,10\neast,20\n").expect("write csv fixture");
        tokio::fs::write(
            &notebook_path,
            serde_json::to_vec_pretty(&empty_notebook()).expect("empty notebook serializes"),
        )
        .await
        .expect("write notebook fixture");

        let jute_state = Arc::new(State::new());
        let notebook_root: NotebookRoot =
            serde_json::from_value(empty_notebook()).expect("empty notebook parses");
        jute_state
            .get_notebook()
            .load(notebook_path.clone(), notebook_root);
        let control = NotebookDaemonControl::new_for_test(
            Arc::new(AgentBridge::new()),
            test_bridge_requester(),
            Arc::clone(&jute_state),
            Arc::new(RecordingWindowOps::default()),
            None,
        );
        {
            let mut state = control.state.lock().await;
            state.current_path = Some(notebook_path.clone());
        }

        let response = control
            .handle(daemon_request(
                jute::commands::DaemonControlCommand::AttachDatasource {
                    name: "sales".to_owned(),
                    path: csv.display().to_string(),
                    group: Some("quarterly".to_owned()),
                },
            ))
            .await;

        assert!(response.ok, "{:?}", response.error);
        let entry: jute::commands::DatasourceEntry =
            serde_json::from_value(response.result.expect("datasource entry result"))
                .expect("datasource entry decodes");

        let serialized: Value = serde_json::from_slice(
            &tokio::fs::read(&notebook_path)
                .await
                .expect("serialized notebook reads"),
        )
        .expect("serialized notebook parses");
        assert_eq!(
            serialized["metadata"]["spur"]["datasources"]["schema_version"],
            json!(1)
        );
        assert_eq!(
            serialized["metadata"]["spur"]["datasources"]["entries"][0]["path"],
            json!(entry.path)
        );
        let relative_path = serialized["metadata"]["spur"]["datasources"]["entries"][0]
            ["workspaceRelativePath"]
            .as_str()
            .expect("workspace-relative path is stored");
        assert!(!std::path::Path::new(relative_path).is_absolute());
        assert!(relative_path.ends_with("sales.csv"));

        let rehydrated = State::new();
        let load_response = jute::commands::handle_daemon_control_request(
            jute::commands::DaemonControlRequest::new(
                jute::commands::DaemonControlCommand::LoadNotebook {
                    path: notebook_path.display().to_string(),
                },
            ),
            &rehydrated,
        )
        .await;

        assert!(load_response.ok, "{:?}", load_response.error);
        assert_eq!(rehydrated.datasource_catalog.lock().list(), vec![entry]);
    }

    fn daemon_request(command: jute::commands::DaemonControlCommand) -> DaemonControlRequest {
        DaemonControlRequest {
            id: None,
            request: jute::commands::DaemonControlRequest::new(command),
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
        recents_changed: StdMutex<Vec<RecentsChangedEvent>>,
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
            self.recents_changed
                .lock()
                .expect("recents_changed lock")
                .len()
        }

        fn recents_changed_events(&self) -> Vec<RecentsChangedEvent> {
            self.recents_changed
                .lock()
                .expect("recents_changed lock")
                .clone()
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

        fn emit_recents_changed(&self, event: &RecentsChangedEvent) {
            self.recents_changed
                .lock()
                .expect("recents_changed lock")
                .push(event.clone());
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
            .handle(daemon_request(jute::commands::DaemonControlCommand::Open {
                path: other_path.display().to_string(),
            }))
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
            .handle(daemon_request(
                jute::commands::DaemonControlCommand::Rename {
                    from: from.display().to_string(),
                    to: to.display().to_string(),
                },
            ))
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
        let events = windows.recents_changed_events();
        assert_eq!(events[0].entries.len(), 1);
        assert_eq!(events[0].entries[0].path, to.display().to_string());
        assert_eq!(events[0].entries[0].pinned, true);
        assert!(events[0].entries[0].is_current);
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
                "command": "open",
                "path": "/tmp/recorded.ipynb"
            }),
        )
        .await
        .expect("control request writes");

        let response = read_frame_value(&mut client)
            .await
            .expect("control response reads");
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "recorded_control_request");
        assert_eq!(control.commands(), vec!["open"]);

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

        fn emit_recents_changed(&self, _event: &RecentsChangedEvent) {}

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
    async fn create_empty_notebook_at_creates_parent_dirs_and_empty_notebook() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-new-at-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("nested").join("analysis.ipynb");

        create_empty_notebook_at(&path)
            .await
            .expect("notebook writes at exact path");

        let contents = tokio::fs::read_to_string(&path)
            .await
            .expect("created notebook reads");
        let notebook: serde_json::Value =
            serde_json::from_str(&contents).expect("created notebook is json");
        assert_eq!(notebook["cells"], json!([]));
        assert_eq!(notebook["metadata"], json!({}));
        assert_eq!(notebook["nbformat"], json!(4));
        assert_eq!(notebook["nbformat_minor"], json!(5));
    }

    #[tokio::test]
    async fn create_empty_notebook_at_refuses_to_overwrite() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-new-at-overwrite-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("analysis.ipynb");
        tokio::fs::write(&path, b"existing")
            .await
            .expect("existing file writes");

        let error = create_empty_notebook_at(&path)
            .await
            .expect_err("existing notebook is not overwritten");

        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(|error| error.kind()),
            Some(std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .expect("existing file reads"),
            "existing"
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
