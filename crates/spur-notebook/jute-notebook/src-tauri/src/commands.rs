//! Invoke handlers for commands callable from the frontend.

use std::{
    env, fs,
    future::Future,
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use dashmap::mapref::entry::Entry;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};
use tauri::{ipc::Channel, AppHandle, WebviewWindow};
use tokio::sync::Mutex;
use tracing::info;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    backend::{
        commands::{self, CompileProgressMode, RunCellEvent},
        local::{environment, KernelUsageInfo, LocalKernel},
        notebook::{
            code_type_for_spec, kernelspec_for, Cell, CellDagMetadata, CodeType,
            FrontendCellMetadata, NotebookRoot,
        },
        wire_protocol::{build_comm_msg, KernelConnection},
    },
    notebook_store::{daemon_cell, CellKind, NotebookDelta, NotebookOp, StoreError},
    ports::{
        go_bootstrap, javascript_bootstrap, notebook_port_root, python_bootstrap, rust_bootstrap,
    },
    state::{
        clear_comm_owners_for_slot, notebook_path_from_slot_id, notebook_slot_id, record_comm_open,
        remove_comm_owner, slot_id_for, window_slot_id, KernelSlot, State,
    },
    Error,
};

/// Re-export so existing callers keep using `commands::DaemonCell`; the type and
/// its root→cell conversion now live in `notebook_store` for correct layering.
pub use crate::notebook_store::DaemonCell;

pub mod venv;

type SaveFuture = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
type SaveWriter = dyn Fn(PathBuf, NotebookRoot) -> SaveFuture + Send + Sync;
type BeforeSaveHook = dyn Fn(&Path, &mut NotebookRoot) + Send + Sync;
const SPUR_NOTEBOOK_PORT_ROOT_ENV: &str = "SPUR_NOTEBOOK_PORT_ROOT";
const SPUR_NOTEBOOK_MCP_SOCKET_ENV: &str = "SPUR_NOTEBOOK_MCP_SOCKET";

/// Snapshot of a stable kernel slot for agent read-side tools.
#[derive(Debug, Clone, Serialize)]
pub struct KernelSlotInfo {
    /// Stable slot ID used by the frontend and Tauri command layer.
    pub kernel_id: String,
    /// Kernel spec name used for the latest successful start.
    pub spec_name: String,
    /// Monotonic in-memory generation for this kernel slot.
    pub generation: u64,
    /// Coarse status for the slot.
    pub status: String,
    /// Kernel CPU usage normalized across available CPUs.
    pub cpu_pct: f32,
    /// Kernel resident memory in MiB.
    pub mem_mb: f32,
}

/// Result returned after a worker delegation is accepted by SPUR.
#[derive(Debug, Clone, Serialize)]
pub struct DelegateResult {
    /// ACP delegation identifier assigned by the SPUR brain.
    pub delegation_id: String,
}

/// Recent notebook entry enriched for the Tauri webview.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecentNotebookEntry {
    /// Absolute notebook path.
    pub path: String,
    /// Last opened time in RFC3339 format.
    pub last_opened: String,
    /// Whether the notebook lives under the scratch directory.
    pub is_scratch: bool,
    /// Whether the notebook is pinned in recents.
    pub pinned: bool,
    /// Whether the path-derived kernel slot has a live kernel.
    pub kernel_alive: bool,
    /// Whether this entry is the daemon's current notebook.
    pub is_current: bool,
}

/// Payload emitted when the recent-notebooks list changes.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecentsChangedEvent {
    /// Current recent-notebook entries.
    pub entries: Vec<RecentNotebookEntry>,
}

/// Recent notebook entry returned by the daemon control protocol.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DaemonRecentEntry {
    /// Absolute notebook path.
    pub path: String,
    /// Last opened time in RFC3339 format.
    pub last_opened: String,
    /// Whether the notebook lives under the scratch directory.
    pub is_scratch: bool,
    /// Whether the notebook is pinned in recents.
    pub pinned: bool,
    /// Whether the path-derived kernel slot has a live kernel, when enriched by Tauri.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kernel_alive: Option<bool>,
    /// Whether this entry is the daemon's current notebook, when enriched by Tauri.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub is_current: Option<bool>,
}

/// Local datasource file type supported by the notebook daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum DatasourceKind {
    /// Comma-separated values file.
    Csv,
    /// Apache Parquet file.
    Parquet,
    /// JSON file.
    Json,
    /// `DuckDB` database file.
    DuckDb,
    /// `SQLite` database file.
    Sqlite,
    /// REST API table-function source.
    ApiTables,
}

/// Column metadata captured for a notebook datasource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Column {
    /// Column name.
    pub name: String,
    /// SQL type reported by the datasource engine.
    pub sql_type: String,
}

/// Table metadata captured for a multi-table notebook datasource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Table {
    /// Table name.
    pub name: String,
    /// Columns discovered for the table.
    pub columns: Vec<Column>,
    /// Row count when known.
    #[ts(type = "number | null")]
    pub row_count: Option<u64>,
}

/// Catalog entry describing one datasource attached to a notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DatasourceEntry {
    /// User-facing datasource name.
    pub name: String,
    /// Datasource file path.
    pub path: String,
    /// Datasource file kind.
    pub kind: DatasourceKind,
    /// Optional UI grouping key.
    pub group: Option<String>,
    /// Columns discovered for the datasource.
    pub columns: Vec<Column>,
    /// Row count when known.
    #[ts(type = "number | null")]
    pub row_count: Option<u64>,
    /// Tables discovered for multi-table datasources.
    #[serde(default)]
    pub tables: Vec<Table>,
}

/// Nango provider metadata summarized for API datasource onboarding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ProviderSummary {
    /// Provider key from the vendored Nango providers snapshot.
    pub name: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Primary provider category.
    pub category: String,
    /// Import tier derived from the provider auth mode.
    pub tier: String,
    /// Nango auth mode string.
    pub auth_mode: String,
}

/// Column metadata returned when previewing OpenAPI-generated API tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct TablePreviewColumn {
    /// Column name.
    pub name: String,
    /// Gateway column type.
    pub ty: String,
    /// `JSONPath` used to extract the column value.
    pub json: String,
}

/// Table metadata returned when previewing OpenAPI-generated API tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct TablePreview {
    /// Generated table name.
    pub name: String,
    /// Request path for the table.
    pub path: String,
    /// `JSONPath` to the response array when detected.
    #[ts(type = "string | null")]
    pub response_path: Option<String>,
    /// Generated columns.
    pub columns: Vec<TablePreviewColumn>,
}

/// Preview payload for OpenAPI-generated API tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct OpenApiTablePreview {
    /// Tables detected from the `OpenAPI` document.
    pub tables: Vec<TablePreview>,
}

/// A daemon control protocol request.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DaemonControlRequest {
    /// Protocol discriminator for notebook daemon control frames.
    pub daemon: String,
    /// Requested daemon operation.
    #[serde(flatten)]
    pub command: DaemonControlCommand,
}

impl DaemonControlRequest {
    /// Build a notebook daemon v1 request.
    pub fn new(command: DaemonControlCommand) -> Self {
        Self {
            daemon: "notebook.v1".to_owned(),
            command,
        }
    }
}

/// Operation encoded in a daemon control request.
#[expect(missing_docs)]
#[expect(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "command", rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum DaemonControlCommand {
    /// Set the focused notebook used by implicit notebook operations.
    SetFocus { notebook_id: String },
    /// Open a notebook file.
    Open { path: String },
    /// Rename a notebook file.
    Rename { from: String, to: String },
    /// Create a scratch notebook.
    New,
    /// Create a notebook at an exact path.
    #[serde(rename = "new_at")]
    #[ts(rename = "new_at")]
    NewAt { path: String },
    /// Reopen the current notebook window.
    Reopen,
    /// Close the current notebook window.
    Close,
    /// Attach a local datasource to the current notebook.
    AttachDatasource {
        name: String,
        path: String,
        group: Option<String>,
    },
    /// Add an API-backed table-function datasource to the current notebook.
    AddApiDatasource { name: String, source: String },
    /// List Nango providers available to the API datasource import wizard.
    ListNangoProviders,
    /// Preview table definitions generated from an `OpenAPI` document.
    PreviewOpenApiTables { spec_text: String },
    /// Compose and attach an API datasource from Nango/OpenAPI import inputs.
    AddApiDatasourceFromImport {
        name: String,
        provider: Option<String>,
        spec_text: Option<String>,
        #[ts(type = "[string, string][]")]
        credentials: Vec<(String, String)>,
    },
    /// Attach and save an API datasource from a table manifest.
    AddApiDatasourceFromManifest {
        name: String,
        manifest_toml: String,
        #[ts(type = "[string, string][]")]
        credentials: Vec<(String, String)>,
    },
    /// Persist a saved API connection template without registering a datasource.
    SaveApiConnectionTemplate {
        name: String,
        #[ts(optional)]
        provider: Option<String>,
        manifest_toml: String,
    },
    /// Complete OAuth browser authorization for a saved API connection.
    OauthConnect { name: String },
    /// Detach a datasource from the current notebook.
    DetachDatasource { name: String },
    /// List datasources attached to the current notebook.
    ListDatasources {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
    },
    /// List globally saved API connection templates.
    ListSavedConnections,
    /// Re-attach a saved connection template into the current notebook.
    AttachSavedConnection {
        name: String,
        #[serde(default)]
        #[ts(type = "[string, string][]")]
        credentials: Vec<(String, String)>,
    },
    /// Delete a saved connection template from the global store.
    DeleteSavedConnection { name: String },
    /// Update a saved connection template in place (edit flow).
    UpdateSavedConnection {
        name: String,
        spec_text: Option<String>,
        #[serde(default)]
        #[ts(type = "[string, string][]")]
        credentials: Vec<(String, String)>,
    },
    /// List daemon recents.
    ListRecents,
    /// Remove a path from daemon recents.
    RemoveFromRecents { path: String },
    /// Set a recent notebook's pin state.
    SetPinned { path: String, pinned: bool },
    /// Replace one cell's source.
    WriteCell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        id: String,
        source: String,
        #[ts(type = "number | null")]
        expected_version: Option<u64>,
        last_edited_by: Option<String>,
    },
    /// Read one cell.
    ReadCell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        id: String,
    },
    /// Insert a cell.
    InsertCell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        kind: CellKind,
        after_id: Option<String>,
        source: String,
        last_edited_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code_type: Option<CodeType>,
    },
    /// Load a notebook from disk into the authoritative store.
    #[serde(rename = "load")]
    #[ts(rename = "load")]
    LoadNotebook { path: String },
    /// Replace the authoritative store with supplied notebook contents.
    ReplaceNotebook {
        path: String,
        contents: NotebookRoot,
    },
    /// Delete one cell.
    DeleteCell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        id: String,
        #[ts(type = "number")]
        expected_version: u64,
    },
    /// Merge cell metadata through the notebook bridge.
    SetCellMetadata {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        id: String,
        patch: serde_json::Value,
        #[ts(type = "number")]
        expected_version: u64,
    },
    /// Return the full notebook root and store version.
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
    },
    /// Apply a UI edit without an optimistic concurrency check.
    ApplyEdit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
        id: String,
        source: String,
    },
    /// Persist the current store snapshot to disk.
    FlushNotebook {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        notebook_id: Option<String>,
    },
}

/// A daemon control protocol response.
#[expect(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DaemonControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub entries: Option<Vec<DaemonRecentEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result: Option<DaemonControlResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<DaemonControlError>,
}

impl DaemonControlResponse {
    fn success(result: DaemonControlResult) -> Self {
        Self {
            ok: true,
            path: None,
            entries: None,
            result: Some(result),
            error: None,
        }
    }

    fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            path: None,
            entries: None,
            result: None,
            error: Some(DaemonControlError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    /// Return the success payload or daemon error.
    pub fn into_result(self) -> Result<DaemonControlResult, DaemonControlError> {
        if self.ok {
            Ok(self.result.unwrap_or(DaemonControlResult::Empty {}))
        } else {
            Err(self.error.unwrap_or_else(|| DaemonControlError {
                code: "daemon_command_failed".to_owned(),
                message: "daemon command failed without an error body".to_owned(),
            }))
        }
    }
}

/// Successful daemon control payloads.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum DaemonControlResult {
    /// No payload.
    Empty,
    /// Notebook mutation delta.
    Delta(NotebookDelta),
    /// Full cell payload.
    Cell(DaemonCell),
    /// Full notebook snapshot.
    Snapshot(DaemonNotebookSnapshot),
    /// Datasource catalog entry.
    Datasource(DatasourceEntry),
    /// Datasource catalog entries.
    Datasources(Vec<DatasourceEntry>),
    /// Nango provider summaries.
    NangoProviders(Vec<ProviderSummary>),
    /// OpenAPI-generated table preview.
    OpenApiTablePreview(OpenApiTablePreview),
    /// Globally saved API connection templates.
    SavedConnections(serde_json::Value),
    /// Saved connection attach payload.
    AttachedSavedConnection(serde_json::Value),
    /// Saved connection delete payload.
    SavedConnectionDeleted(serde_json::Value),
}

/// Full notebook snapshot returned by daemon control.
#[expect(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DaemonNotebookSnapshot {
    pub root: NotebookRoot,
    #[ts(type = "number")]
    pub version: u64,
}

/// Error payload returned by daemon control failures.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DaemonControlError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct LastNotebookRecord {
    path: PathBuf,
}

/// Coordinates disk saves so only one notebook write runs at a time.
#[derive(Clone)]
pub struct SaveCoordinator {
    inner: Arc<Mutex<SaveState>>,
    writer: Arc<SaveWriter>,
    before_save: Option<Arc<BeforeSaveHook>>,
}

#[derive(Default)]
struct SaveState {
    in_flight: bool,
    queued: Option<PendingSave>,
}

struct PendingSave {
    path: PathBuf,
    contents: NotebookRoot,
}

impl Default for SaveCoordinator {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SaveState::default())),
            writer: Arc::new(|path, contents| {
                Box::pin(async move { atomic_write_notebook(&path, &contents).await })
            }),
            before_save: None,
        }
    }
}

impl SaveCoordinator {
    /// Queue and persist notebook contents to disk.
    pub async fn save(&self, path: PathBuf, mut contents: NotebookRoot) -> Result<(), Error> {
        if let Some(before_save) = self.before_save.as_ref() {
            before_save(&path, &mut contents);
        }

        let mut state = self.inner.lock().await;
        state.queued = Some(PendingSave { path, contents });
        if state.in_flight {
            return Ok(());
        }
        state.in_flight = true;
        drop(state);

        loop {
            let pending = {
                let mut state = self.inner.lock().await;
                state.queued.take().expect("save queue must contain work")
            };

            let result = (self.writer)(pending.path, pending.contents).await;
            let mut state = self.inner.lock().await;
            if let Err(error) = result {
                state.in_flight = false;
                return Err(error);
            }

            if state.queued.is_none() {
                state.in_flight = false;
                return Ok(());
            }
        }
    }

    /// Build a coordinator that patches notebook metadata immediately before
    /// writes use the normal atomic-save path.
    pub fn with_before_save<F>(before_save: F) -> Self
    where
        F: Fn(&Path, &mut NotebookRoot) + Send + Sync + 'static,
    {
        Self {
            before_save: Some(Arc::new(before_save)),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_writer_for_test<F>(writer: F) -> Self
    where
        F: Fn(PathBuf, NotebookRoot) -> SaveFuture + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(Mutex::new(SaveState::default())),
            writer: Arc::new(writer),
            before_save: None,
        }
    }
}

async fn atomic_write_notebook(path: &Path, contents: &NotebookRoot) -> Result<(), Error> {
    let path = path.to_owned();
    let contents = contents.clone();
    tokio::task::spawn_blocking(move || atomic_write_notebook_blocking(&path, &contents))
        .await
        .map_err(|error| Error::Filesystem(io::Error::other(error)))?
}

fn atomic_write_notebook_blocking(path: &Path, contents: &NotebookRoot) -> Result<(), Error> {
    atomic_write_notebook_blocking_with_hook(path, contents, |_| Ok(()))
}

fn atomic_write_notebook_blocking_with_hook<F>(
    path: &Path,
    contents: &NotebookRoot,
    before_rename: F,
) -> Result<(), Error>
where
    F: FnOnce(&Path) -> Result<(), Error>,
{
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::Filesystem(io::Error::new(
                io::ErrorKind::InvalidInput,
                "notebook path must have a file name",
            ))
        })?;
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(Error::Filesystem)?;
        serde_json::to_writer_pretty(&mut file, contents)?;
        writeln!(&mut file).map_err(Error::Filesystem)?;
        file.sync_all().map_err(Error::Filesystem)?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(error) = before_rename(&temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    fs::rename(&temp_path, path).map_err(Error::Filesystem)
}

fn slot_id_for_window(window: &WebviewWindow) -> String {
    notebook_path_from_window(window)
        .map(|path| notebook_slot_id(&path))
        .unwrap_or_else(|| window_slot_id(window.label()))
}

fn notebook_path_from_window(window: &WebviewWindow) -> Option<String> {
    let url = window.url().ok()?;
    url.query_pairs()
        .find_map(|(key, value)| (key == "path").then(|| value.into_owned()))
        .filter(|path| !path.is_empty())
}

fn daemon_socket_path_from_args() -> Result<PathBuf, Error> {
    let mut args = env::args();
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from).ok_or_else(|| {
                Error::NotebookDaemon("--socket requires a notebook daemon path".to_owned())
            });
        }
    }
    Err(Error::NotebookDaemon(
        "notebook daemon socket path was not provided".to_owned(),
    ))
}

#[cfg(unix)]
/// Write one length-prefixed daemon-control frame.
pub async fn write_daemon_frame<W>(writer: &mut W, bytes: &[u8]) -> Result<(), Error>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::NotebookDaemon(format!(
            "daemon request frame is too large: {} bytes",
            bytes.len()
        )));
    }
    use tokio::io::AsyncWriteExt as _;
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(Error::Filesystem)?;
    writer.write_all(bytes).await.map_err(Error::Filesystem)?;
    writer.flush().await.map_err(Error::Filesystem)
}

#[cfg(unix)]
/// Read one length-prefixed daemon-control frame.
pub async fn read_daemon_frame<R>(reader: &mut R) -> Result<Vec<u8>, Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    use tokio::io::AsyncReadExt as _;
    let mut len = [0_u8; 4];
    reader
        .read_exact(&mut len)
        .await
        .map_err(Error::Filesystem)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(Error::NotebookDaemon(format!(
            "daemon response frame is too large: {len} bytes"
        )));
    }
    let mut bytes = vec![0_u8; len];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(Error::Filesystem)?;
    Ok(bytes)
}

#[cfg(unix)]
/// Send one daemon-control request to a Unix socket and read one response frame.
pub async fn send_daemon_control_to(
    socket_path: &Path,
    request: &DaemonControlRequest,
) -> Result<DaemonControlResponse, Error> {
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(Error::Filesystem)?;
    write_daemon_frame(&mut stream, &serde_json::to_vec(request)?).await?;
    let bytes = read_daemon_frame(&mut stream).await?;
    let response: DaemonControlResponse = serde_json::from_slice(&bytes)?;
    if response.ok {
        Ok(response)
    } else {
        let message = response
            .error
            .as_ref()
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "daemon command failed without an error body".to_owned());
        Err(Error::NotebookDaemon(message))
    }
}

#[cfg(not(unix))]
/// Send one daemon-control request to a Unix socket and read one response frame.
pub async fn send_daemon_control_to(
    _socket_path: &Path,
    _request: &DaemonControlRequest,
) -> Result<DaemonControlResponse, Error> {
    Err(Error::NotebookDaemon(
        "notebook daemon socket commands are only available on Unix platforms".to_string(),
    ))
}

#[cfg(unix)]
#[tauri::command]
/// Send a typed daemon-control command through the app's daemon socket.
pub async fn daemon_control(
    cmd: DaemonControlCommand,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<DaemonControlResponse, Error> {
    let socket_path = daemon_socket_path_from_args()?;
    let enrich_recents = matches!(cmd, DaemonControlCommand::ListRecents);
    let request = DaemonControlRequest::new(cmd);
    let mut response = send_daemon_control_to(&socket_path, &request).await?;
    if enrich_recents {
        enrich_daemon_recent_entries(&mut response, &state).await?;
    }
    Ok(response)
}

#[cfg(not(unix))]
#[tauri::command]
/// Send a typed daemon-control command through the app's daemon socket.
pub async fn daemon_control(
    _cmd: DaemonControlCommand,
    _state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<DaemonControlResponse, Error> {
    Err(Error::NotebookDaemon(
        "notebook daemon socket commands are only available on Unix platforms".to_string(),
    ))
}

/// Handle notebook-store daemon control requests inside the Tauri process.
pub async fn handle_daemon_control_request(
    request: DaemonControlRequest,
    state: &State,
) -> DaemonControlResponse {
    if request.daemon != "notebook.v1" {
        return DaemonControlResponse::failure(
            "invalid_control_message",
            format!("unsupported daemon discriminator: {}", request.daemon),
        );
    }

    match handle_daemon_control_inner(request.command, state).await {
        Ok(result) => DaemonControlResponse::success(result),
        Err(error) => error,
    }
}

/// Replace the active notebook and refresh the in-memory datasource catalog
/// from the replacement document's metadata.
pub fn replace_notebook_and_hydrate_catalog(
    state: &State,
    path: PathBuf,
    contents: NotebookRoot,
) -> NotebookDelta {
    hydrate_datasource_catalog_from_root(state, path.as_path(), &contents);
    state.focus_notebook_path(&path).replace(path, contents)
}

/// Replace the notebook store for a path without changing the focused notebook.
pub fn replace_notebook_for_path_and_hydrate_catalog(
    state: &State,
    path: PathBuf,
    contents: NotebookRoot,
) -> NotebookDelta {
    hydrate_datasource_catalog_from_root(state, path.as_path(), &contents);
    state.notebook_for_path(&path).replace(path, contents)
}

fn hydrate_datasource_catalog_from_root(state: &State, path: &Path, root: &NotebookRoot) {
    let catalog =
        crate::state::DatasourceCatalog::hydrate_from_metadata(&root.metadata, Some(path));
    let entries = catalog.list();
    state.replace_datasource_catalog_for_path(path, catalog);
    state.emit_datasources_changed(entries);
}

async fn handle_daemon_control_inner(
    command: DaemonControlCommand,
    state: &State,
) -> Result<DaemonControlResult, DaemonControlResponse> {
    match command {
        DaemonControlCommand::SetFocus { notebook_id } => {
            if notebook_id.is_empty() {
                return Err(DaemonControlResponse::failure(
                    "invalid_params",
                    "set_focus notebook_id must not be empty",
                ));
            }
            state.set_focused_notebook_target(&notebook_id);
            Ok(DaemonControlResult::Empty)
        }
        DaemonControlCommand::WriteCell {
            notebook_id,
            id,
            source,
            expected_version,
            last_edited_by,
        } => {
            validate_cell_id(&id)?;
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            notebook
                .apply(NotebookOp::WriteCell {
                    id,
                    source,
                    expected_version,
                    last_edited_by,
                })
                .map(DaemonControlResult::Delta)
                .map_err(store_error_response)
        }
        DaemonControlCommand::ReadCell { notebook_id, id } => {
            validate_cell_id(&id)?;
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            let (root, _version) = notebook.snapshot();
            read_daemon_cell(&root, &id).map(DaemonControlResult::Cell)
        }
        DaemonControlCommand::InsertCell {
            notebook_id,
            kind,
            after_id,
            source,
            last_edited_by,
            code_type,
        } => {
            if matches!(after_id.as_deref(), Some("")) {
                return Err(DaemonControlResponse::failure(
                    "invalid_params",
                    "insert_cell after_id must not be empty",
                ));
            }
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            notebook
                .apply(NotebookOp::InsertCell {
                    kind,
                    after_id,
                    source,
                    last_edited_by,
                    code_type,
                })
                .map(DaemonControlResult::Delta)
                .map_err(store_error_response)
        }
        DaemonControlCommand::LoadNotebook { path } => {
            let contents = tokio::fs::read_to_string(&path).await.map_err(|error| {
                DaemonControlResponse::failure("load_failed", error.to_string())
            })?;
            let root: NotebookRoot = serde_json::from_str(&contents).map_err(|error| {
                DaemonControlResponse::failure("load_failed", error.to_string())
            })?;
            hydrate_datasource_catalog_from_root(state, Path::new(&path), &root);
            let notebook = state.focus_notebook_path(&path);
            let delta = notebook.load(PathBuf::from(path), root);
            Ok(DaemonControlResult::Delta(delta))
        }
        DaemonControlCommand::ReplaceNotebook { path, contents } => {
            let delta = replace_notebook_and_hydrate_catalog(state, PathBuf::from(path), contents);
            Ok(DaemonControlResult::Delta(delta))
        }
        DaemonControlCommand::ListDatasources { notebook_id } => {
            let entries = state.list_datasources_for_optional_target(notebook_id.as_deref());
            Ok(DaemonControlResult::Datasources(entries))
        }
        DaemonControlCommand::DeleteCell {
            notebook_id,
            id,
            expected_version,
        } => {
            validate_cell_id(&id)?;
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            notebook
                .apply(NotebookOp::DeleteCell {
                    id,
                    expected_version,
                })
                .map(DaemonControlResult::Delta)
                .map_err(store_error_response)
        }
        DaemonControlCommand::SetCellMetadata {
            notebook_id,
            id,
            patch,
            expected_version,
        } => {
            validate_cell_id(&id)?;
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            if patch
                .get("spur")
                .and_then(|spur| spur.get("datasource_setup"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                return notebook
                    .apply(NotebookOp::MarkDatasourceSetupCell {
                        id,
                        expected_version,
                    })
                    .map(DaemonControlResult::Delta)
                    .map_err(store_error_response);
            }
            if let Some(dag) = patch.get("spur").and_then(|spur| spur.get("dag")) {
                let dag =
                    serde_json::from_value::<CellDagMetadata>(dag.clone()).map_err(|error| {
                        DaemonControlResponse::failure(
                            "invalid_params",
                            format!("invalid spur dag metadata patch: {error}"),
                        )
                    })?;
                return notebook
                    .apply(NotebookOp::SetSpurDagMetadata {
                        id,
                        patch: dag,
                        expected_version,
                    })
                    .map(DaemonControlResult::Delta)
                    .map_err(store_error_response);
            }
            if let Some(code_type) = patch.get("spur").and_then(|spur| spur.get("code_type")) {
                let code_type =
                    serde_json::from_value::<CodeType>(code_type.clone()).map_err(|error| {
                        DaemonControlResponse::failure(
                            "invalid_params",
                            format!("invalid spur code_type metadata patch: {error}"),
                        )
                    })?;
                return notebook
                    .apply(NotebookOp::SetSpurCodeTypeMetadata {
                        id,
                        code_type,
                        expected_version,
                    })
                    .map(DaemonControlResult::Delta)
                    .map_err(store_error_response);
            }
            if let Some(frontend) = patch.get("spur").and_then(|spur| spur.get("frontend")) {
                let frontend = serde_json::from_value::<FrontendCellMetadata>(frontend.clone())
                    .map_err(|error| {
                        DaemonControlResponse::failure(
                            "invalid_params",
                            format!("invalid spur frontend metadata patch: {error}"),
                        )
                    })?;
                return notebook
                    .apply(NotebookOp::SetSpurFrontendMetadata {
                        id,
                        patch: frontend,
                        expected_version,
                    })
                    .map(DaemonControlResult::Delta)
                    .map_err(store_error_response);
            }
            let patch = serde_json::from_value(patch).map_err(|error| {
                DaemonControlResponse::failure(
                    "invalid_params",
                    format!("invalid cell metadata patch: {error}"),
                )
            })?;
            notebook
                .apply(NotebookOp::SetJuteDeckMetadata {
                    id,
                    patch,
                    expected_version,
                })
                .map(DaemonControlResult::Delta)
                .map_err(store_error_response)
        }
        DaemonControlCommand::Snapshot { notebook_id } => {
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            let (root, version) = notebook.snapshot();
            Ok(DaemonControlResult::Snapshot(DaemonNotebookSnapshot {
                root,
                version,
            }))
        }
        DaemonControlCommand::ApplyEdit {
            notebook_id,
            id,
            source,
        } => {
            validate_cell_id(&id)?;
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            notebook
                .apply(NotebookOp::ApplyEdit { id, source })
                .map(DaemonControlResult::Delta)
                .map_err(store_error_response)
        }
        DaemonControlCommand::FlushNotebook { notebook_id } => {
            let notebook = state.notebook_for_optional_target(notebook_id.as_deref());
            notebook
                .flush()
                .await
                .map(|()| DaemonControlResult::Empty)
                .map_err(|error| DaemonControlResponse::failure("flush_failed", error.to_string()))
        }
        command => Err(DaemonControlResponse::failure(
            "unsupported_daemon_command",
            format!("daemon command is not handled by the notebook store: {command:?}"),
        )),
    }
}

#[expect(clippy::result_large_err)]
fn validate_cell_id(id: &str) -> Result<(), DaemonControlResponse> {
    if id.is_empty() {
        Err(DaemonControlResponse::failure(
            "invalid_params",
            "cell id must not be empty",
        ))
    } else {
        Ok(())
    }
}

fn store_error_response(error: StoreError) -> DaemonControlResponse {
    match error {
        StoreError::OptimisticConcurrency { expected, actual } => DaemonControlResponse::failure(
            "stale_version",
            format!("expected version {expected}, actual version {actual}"),
        ),
        StoreError::CellNotFound { id } => {
            DaemonControlResponse::failure("cell_not_found", format!("cell not found: {id}"))
        }
        StoreError::NotCodeCell { id } => {
            DaemonControlResponse::failure("not_code_cell", format!("cell is not code: {id}"))
        }
    }
}

/// Thin daemon-layer wrapper over [`daemon_cell`], mapping a missing cell to the
/// daemon control error the read path returns.
#[expect(clippy::result_large_err)]
fn read_daemon_cell(root: &NotebookRoot, id: &str) -> Result<DaemonCell, DaemonControlResponse> {
    daemon_cell(root, id).ok_or_else(|| {
        DaemonControlResponse::failure("cell_not_found", format!("cell not found: {id}"))
    })
}

fn home_dir() -> Result<PathBuf, Error> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(home) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home));
    }
    #[cfg(windows)]
    if let (Some(drive), Some(path)) = (env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH")) {
        let mut home = PathBuf::from(drive);
        home.push(path);
        return Ok(home);
    }
    Err(Error::Filesystem(io::Error::new(
        io::ErrorKind::NotFound,
        "could not resolve home directory",
    )))
}

fn notebooks_dir() -> Result<PathBuf, Error> {
    Ok(home_dir()?.join(".spur").join("notebooks"))
}

fn scratch_dir() -> Result<PathBuf, Error> {
    Ok(home_dir()?.join(".spur").join("scratch"))
}

fn last_notebook_record_path() -> Result<PathBuf, Error> {
    Ok(notebooks_dir()?.join("last.json"))
}

async fn load_current_notebook_path() -> Result<Option<PathBuf>, Error> {
    let record_path = last_notebook_record_path()?;
    let bytes = match tokio::fs::read(&record_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Filesystem(error)),
    };
    let record: LastNotebookRecord = serde_json::from_slice(&bytes)?;
    Ok(Some(record.path))
}

async fn load_current_notebook_path_normalized() -> Result<Option<PathBuf>, Error> {
    match load_current_notebook_path().await? {
        Some(path) => normalize_path(&path).await.map(Some),
        None => Ok(None),
    }
}

async fn normalize_path(path: &Path) -> Result<PathBuf, Error> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => lexical_normalize(path),
        Err(error) => Err(Error::Filesystem(error)),
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, Error> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().map_err(Error::Filesystem)?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !is_root_or_prefix_only(&normalized) {
                    normalized.pop();
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

fn is_root_or_prefix_only(path: &Path) -> bool {
    let mut saw_anchor = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => saw_anchor = true,
            _ => return false,
        }
    }
    saw_anchor
}

fn apply_notebook_port_root_env(
    kernel_spec: &mut environment::KernelSpec,
    port_root: Option<&Path>,
) {
    if let Some(root) = port_root {
        kernel_spec.env.insert(
            SPUR_NOTEBOOK_PORT_ROOT_ENV.to_owned(),
            root.display().to_string(),
        );
    }
}

fn apply_notebook_mcp_socket_env(kernel_spec: &mut environment::KernelSpec) {
    let Ok(socket_path) = daemon_socket_path_from_args() else {
        return;
    };
    kernel_spec.env.insert(
        SPUR_NOTEBOOK_MCP_SOCKET_ENV.to_owned(),
        socket_path.display().to_string(),
    );
}

/// Start a local Jupyter kernel by spec name.
pub async fn start_local_kernel(
    spec_name: &str,
    port_root: Option<&std::path::Path>,
    working_dir: Option<&std::path::Path>,
) -> Result<LocalKernel, Error> {
    // Temporary hack to just start a kernel locally with ZeroMQ.
    let kernels = environment::list_kernels(None).await;
    let mut kernel_spec = match kernels
        .iter()
        .find(|(path, _spec)| path.file_name().and_then(|s| s.to_str()) == Some(spec_name))
    {
        Some((_, kernel_spec)) => kernel_spec.clone(),
        None => {
            return Err(Error::KernelConnect(format!(
                "no kernel named {spec_name:?} found"
            )))
        }
    };

    if let Some(command) = kernel_spec.argv.first_mut() {
        if command == "python" {
            if let Ok(python_path) = env::var("PYTHON_PATH") {
                *command = python_path;
            } else {
                // Temporary hack
                *command = "/opt/homebrew/bin/python3.11".into();
            }
        }
    }

    apply_notebook_port_root_env(&mut kernel_spec, port_root);
    apply_notebook_mcp_socket_env(&mut kernel_spec);
    let kernel = LocalKernel::start(&kernel_spec, working_dir).await?;

    let info = commands::kernel_info(kernel.conn()).await?;
    info!(banner = info.banner, "started new jute kernel");

    Ok(kernel)
}

/// Install a fresh kernel in a slot, returning its generation and any previous kernel.
pub fn install_kernel_in_slot(
    state: &State,
    slot_id: &str,
    spec_name: String,
    kernel: LocalKernel,
) -> (u64, Option<LocalKernel>) {
    match state.kernels.entry(slot_id.to_owned()) {
        Entry::Occupied(mut entry) => {
            let slot = entry.get_mut();
            let previous_kernel = slot.kernel.take();
            let generation = slot.replace_kernel(kernel, spec_name);
            (generation, previous_kernel)
        }
        Entry::Vacant(entry) => {
            let slot = KernelSlot::with_kernel(kernel, spec_name);
            let generation = slot.generation();
            entry.insert(slot);
            (generation, None)
        }
    }
}

/// Await a kernel's death signal, then invoke `restart` exactly once.
pub(crate) async fn supervise_until_dead<F, Fut>(
    liveness: tokio_util::sync::CancellationToken,
    restart: F,
) where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    liveness.cancelled().await;
    if let Err(error) = restart().await {
        tracing::error!(?error, "kernel supervisor restart failed");
    }
}

fn spawn_kernel_supervisor(
    state: &Arc<State>,
    slot_id: &str,
    spec_name: &str,
    liveness: tokio_util::sync::CancellationToken,
) {
    let sup_state = Arc::clone(state);
    let sup_slot = slot_id.to_owned();
    let sup_spec = spec_name.to_owned();
    tokio::spawn(async move {
        supervise_until_dead(liveness, || async move {
            restart_kernel_in_slot(&sup_state, &sup_slot, &sup_spec)
                .await
                .map(|_| ())
        })
        .await;
    });
}

/// Restart the kernel bound to `slot_id`: kill the prior process, start a fresh
/// kernel, re-inject the port bootstrap, and install it into the slot. Returns
/// the new slot generation.
pub async fn restart_kernel_in_slot(
    state: &Arc<State>,
    slot_id: &str,
    spec_name: &str,
) -> Result<u64, Error> {
    let mut prior = take_kernel_from_slot(state, slot_id)?;
    prior.kill().await?;

    let nb_path = notebook_path_from_slot_id(slot_id, spec_name).map(std::path::Path::new);
    let port_root = nb_path.map(notebook_port_root);
    let working_dir = nb_path.and_then(|p| p.parent());
    let mut kernel = start_local_kernel(spec_name, port_root.as_deref(), working_dir).await?;
    if let Err(error) = inject_port_bootstrap(kernel.conn(), spec_name).await {
        let _ = kernel.kill().await;
        return Err(error);
    }
    let liveness = kernel.conn().liveness_token();
    let (generation, _previous) =
        install_kernel_in_slot(state, slot_id, spec_name.to_owned(), kernel);
    spawn_kernel_supervisor(state, slot_id, spec_name, liveness);
    Ok(generation)
}

/// Delegate AI work to a SPUR worker.
///
/// The jute shell exposes this command name so frontend deck commands fail with
/// a structured bridge error instead of Tauri's generic "command not found"
/// when the SPUR brain delegation path has not been wired into this process.
#[tauri::command]
pub fn spur_delegate_to_worker(
    task: String,
    worker_type: String,
    tool_allowlist: Vec<String>,
) -> Result<DelegateResult, String> {
    let _ = (task, worker_type, tool_allowlist);
    Err("delegate_to_worker bridge not wired; see plan task 11".to_owned())
}

fn take_kernel_if_present(state: &State, slot_id: &str) -> Option<LocalKernel> {
    let mut slot = state.kernels.get_mut(slot_id)?;
    slot.kernel.take()
}

/// Remove and return the kernel currently bound to a slot, or `KernelDisconnect`.
pub fn take_kernel_from_slot(state: &State, slot_id: &str) -> Result<LocalKernel, Error> {
    let mut slot = state
        .kernels
        .get_mut(slot_id)
        .ok_or(Error::KernelDisconnect)?;
    slot.kernel.take().ok_or(Error::KernelDisconnect)
}

fn restore_kernel_to_slot(state: &State, slot_id: &str, kernel: LocalKernel) {
    if let Some(mut slot) = state.kernels.get_mut(slot_id) {
        slot.kernel = Some(kernel);
    }
}

fn kernel_connection_for_slot(
    state: &State,
    slot_id: &str,
) -> Result<crate::backend::KernelConnection, Error> {
    let slot = state.kernels.get(slot_id).ok_or(Error::KernelDisconnect)?;
    Ok(slot
        .kernel
        .as_ref()
        .ok_or(Error::KernelDisconnect)?
        .conn()
        .clone())
}

/// Return the spec name recorded for an existing kernel slot.
pub fn spec_name_for_slot(state: &State, slot_id: &str) -> Result<String, Error> {
    let slot = state.kernels.get(slot_id).ok_or(Error::KernelDisconnect)?;
    Ok(slot.spec_name().to_owned())
}

struct KernelProcessUsage {
    cpu_consumed: f32,
    cpu_available: f32,
    memory_consumed: f32,
    memory_available: f32,
}

async fn kernel_process_usage(pid: Pid) -> Option<KernelProcessUsage> {
    let mut system = System::new_all();
    system.refresh_all();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    system.refresh_process(pid);

    system.process(pid).map(|process| KernelProcessUsage {
        cpu_consumed: process.cpu_usage(),
        cpu_available: system.cpus().len() as f32,
        memory_consumed: process.memory() as f32,
        memory_available: system.total_memory() as f32,
    })
}

/// Measure the kernel's CPU and memory usage as a percentage of total system
/// resources.
#[tauri::command]
pub async fn kernel_usage_info(
    kernel_id: &str,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<KernelUsageInfo, Error> {
    let pid: Pid = {
        let slot = state.kernels.get(kernel_id).ok_or(Error::KernelNotFound)?;
        let kernel = slot.kernel.as_ref().ok_or(Error::KernelNotFound)?;
        Pid::from_u32(kernel.pid().ok_or(Error::KernelProcessNotFound)?)
    };

    if let Some(usage) = kernel_process_usage(pid).await {
        Ok(KernelUsageInfo {
            cpu_consumed: usage.cpu_consumed,
            cpu_available: usage.cpu_available,
            memory_consumed: usage.memory_consumed,
            memory_available: usage.memory_available,
        })
    } else {
        Err(Error::KernelProcessNotFound)
    }
}

/// Return read-side metadata for a stable kernel slot.
#[tauri::command]
pub async fn kernel_slot_info(
    kernel_id: &str,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<KernelSlotInfo, Error> {
    kernel_slot_info_for_state(kernel_id, &state).await
}

/// Read-only kernel slot info usable outside the Tauri command surface.
pub async fn kernel_slot_info_for_state(
    kernel_id: &str,
    state: &State,
) -> Result<KernelSlotInfo, Error> {
    let (spec_name, generation, pid) = {
        let slot = state.kernels.get(kernel_id).ok_or(Error::KernelNotFound)?;
        (
            slot.spec_name().to_owned(),
            slot.generation(),
            slot.kernel.as_ref().and_then(|kernel| kernel.pid()),
        )
    };

    let (status, cpu_pct, mem_mb) = match pid {
        Some(pid) => match kernel_process_usage(Pid::from_u32(pid)).await {
            Some(usage) => {
                let cpu_pct = if usage.cpu_available > 0.0 {
                    (usage.cpu_consumed / usage.cpu_available) * 100.0
                } else {
                    0.0
                };
                (
                    "idle".to_owned(),
                    cpu_pct,
                    usage.memory_consumed / 1024.0 / 1024.0,
                )
            }
            None => ("dead".to_owned(), 0.0, 0.0),
        },
        None => ("dead".to_owned(), 0.0, 0.0),
    };

    Ok(KernelSlotInfo {
        kernel_id: kernel_id.to_owned(),
        spec_name,
        generation,
        status,
        cpu_pct,
        mem_mb,
    })
}

async fn kernel_alive_for_notebook(path: &str, state: &State) -> bool {
    let slot_id = notebook_slot_id(path);
    kernel_slot_info_for_state(&slot_id, state)
        .await
        .map(|info| info.status != "dead")
        .unwrap_or(false)
}

async fn enrich_daemon_recent_entries(
    response: &mut DaemonControlResponse,
    state: &State,
) -> Result<(), Error> {
    let current_path = load_current_notebook_path_normalized().await?;
    let Some(entries) = response.entries.as_mut() else {
        return Ok(());
    };
    for entry in entries {
        let normalized_path = normalize_path(Path::new(&entry.path)).await?;
        let path = normalized_path.display().to_string();
        entry.kernel_alive = Some(kernel_alive_for_notebook(&path, state).await);
        entry.is_current = Some(current_path.as_ref() == Some(&normalized_path));
        entry.path = path;
    }
    Ok(())
}

/// Move a notebook file to the OS trash unless it is currently loaded.
#[tauri::command]
pub async fn move_notebook_to_trash(path: String) -> Result<(), Error> {
    let path = normalize_path(Path::new(&path)).await?;
    let current_path = load_current_notebook_path_normalized().await?;
    move_notebook_to_trash_with_current_path(&path, current_path.as_deref(), trash_path)
}

fn move_notebook_to_trash_with_current_path<F>(
    path: &Path,
    current_path: Option<&Path>,
    trasher: F,
) -> Result<(), Error>
where
    F: FnOnce(&Path) -> Result<(), Error>,
{
    if current_path == Some(path) {
        return Err(Error::NotebookDaemon(
            "cannot move the currently loaded notebook to trash".to_owned(),
        ));
    }
    trasher(path)
}

fn trash_path(path: &Path) -> Result<(), Error> {
    trash::delete(path).map_err(|error| Error::Filesystem(io::Error::other(error.to_string())))
}

/// Reveal a notebook in the platform file manager.
#[tauri::command]
pub async fn reveal_notebook_in_finder(path: String) -> Result<(), Error> {
    reveal_notebook_path(Path::new(&path)).await
}

async fn reveal_notebook_path(path: &Path) -> Result<(), Error> {
    let mut command = reveal_command(path)?;
    let status = command.status().await.map_err(Error::Subprocess)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Subprocess(io::Error::other(format!(
            "file manager exited with status {status}"
        ))))
    }
}

#[cfg_attr(
    any(target_os = "macos", target_os = "windows"),
    expect(clippy::unnecessary_wraps)
)]
fn reveal_command(path: &Path) -> Result<tokio::process::Command, Error> {
    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("open");
        command.arg("-R").arg(path);
        Ok(command)
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = tokio::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        Ok(command)
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let parent = path.parent().ok_or_else(|| {
            Error::Filesystem(io::Error::new(
                io::ErrorKind::InvalidInput,
                "notebook path must have a parent directory",
            ))
        })?;
        let mut command = tokio::process::Command::new("xdg-open");
        command.arg(parent);
        Ok(command)
    }
}

/// Move all inactive scratch notebooks to the OS trash.
#[tauri::command]
pub async fn discard_scratch_notebooks() -> Result<usize, Error> {
    let scratch_dir = scratch_dir()?;
    let current_path = load_current_notebook_path_normalized().await?;
    discard_scratch_notebooks_in(&scratch_dir, current_path.as_deref(), trash_path).await
}

async fn discard_scratch_notebooks_in<F>(
    scratch_dir: &Path,
    current_path: Option<&Path>,
    mut trasher: F,
) -> Result<usize, Error>
where
    F: FnMut(&Path) -> Result<(), Error>,
{
    let mut entries = match tokio::fs::read_dir(scratch_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(Error::Filesystem(error)),
    };

    let mut trashed = 0;
    while let Some(entry) = entries.next_entry().await.map_err(Error::Filesystem)? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("ipynb") {
            continue;
        }
        let path = normalize_path(&path).await?;
        if current_path == Some(path.as_path()) {
            continue;
        }
        trasher(&path)?;
        trashed += 1;
    }
    Ok(trashed)
}

/// Start a new Jupyter kernel.
#[tauri::command]
pub async fn start_kernel(
    app: AppHandle,
    spec_name: &str,
    window: WebviewWindow,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<String, Error> {
    // TODO: Save the client in a better place.
    // let client = JupyterClient::new("", "")?;

    let slot_id = slot_id_for_window(&window);
    let notebook_path = load_current_notebook_path_normalized().await?;
    let port_root = notebook_path.as_deref().map(notebook_port_root);
    let working_dir = notebook_path.as_deref().and_then(|p| p.parent());
    if let Some(mut kernel) = take_kernel_if_present(&state, &slot_id) {
        if let Err(error) = kernel.kill().await {
            restore_kernel_to_slot(&state, &slot_id, kernel);
            return Err(error);
        }
        clear_comm_owners_for_slot(&state, &slot_id);
    }

    crate::kernel_provision::ensure_python3_kernelspec(&app).await?;
    let mut kernel = start_local_kernel(spec_name, port_root.as_deref(), working_dir).await?;
    if let Err(error) = inject_port_bootstrap(kernel.conn(), spec_name).await {
        let _ = kernel.kill().await;
        return Err(error);
    }
    let liveness = kernel.conn().liveness_token();
    let (generation, _previous_kernel) =
        install_kernel_in_slot(&state, &slot_id, spec_name.to_owned(), kernel);
    spawn_kernel_supervisor(state.inner(), &slot_id, spec_name, liveness);
    info!(slot_id = %slot_id, generation, "started jute kernel slot");

    Ok(slot_id)
}

/// Restart a Jupyter kernel in an existing stable slot.
#[tauri::command]
pub async fn restart_kernel(
    slot_id: &str,
    spec_name: Option<String>,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<String, Error> {
    info!("restarting jute kernel slot {slot_id}");
    let next_spec_name = match spec_name {
        Some(spec_name) => spec_name,
        None => spec_name_for_slot(&state, slot_id)?,
    };
    let nb_path_restart =
        notebook_path_from_slot_id(slot_id, &next_spec_name).map(std::path::Path::new);
    let port_root = nb_path_restart.map(notebook_port_root);
    let working_dir_restart = nb_path_restart.and_then(|p| p.parent());

    let mut kernel = take_kernel_from_slot(&state, slot_id)?;
    if let Err(error) = kernel.kill().await {
        restore_kernel_to_slot(&state, slot_id, kernel);
        return Err(error);
    }
    clear_comm_owners_for_slot(&state, slot_id);

    let mut kernel =
        start_local_kernel(&next_spec_name, port_root.as_deref(), working_dir_restart).await?;
    if let Err(error) = inject_port_bootstrap(kernel.conn(), &next_spec_name).await {
        let _ = kernel.kill().await;
        return Err(error);
    }
    let liveness = kernel.conn().liveness_token();
    let (generation, _previous_kernel) =
        install_kernel_in_slot(&state, slot_id, next_spec_name.clone(), kernel);
    spawn_kernel_supervisor(state.inner(), slot_id, &next_spec_name, liveness);
    info!(slot_id = %slot_id, generation, "restarted jute kernel slot");

    Ok(slot_id.to_owned())
}

/// Stop a Jupyter kernel.
#[tauri::command]
pub async fn stop_kernel(kernel_id: &str, state: tauri::State<'_, State>) -> Result<(), Error> {
    info!("stopping jute kernel slot {kernel_id}");
    let mut kernel = take_kernel_from_slot(&state, kernel_id)?;
    if let Err(error) = kernel.kill().await {
        restore_kernel_to_slot(&state, kernel_id, kernel);
        return Err(error);
    }
    clear_comm_owners_for_slot(&state, kernel_id);
    Ok(())
}

/// Get the contents of a Jupyter notebook on disk.
#[tauri::command]
pub async fn get_notebook(path: &str) -> Result<NotebookRoot, Error> {
    info!("getting notebook at {path}");

    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(Error::Filesystem)?;
    Ok(serde_json::from_str(&contents)?)
}

#[derive(Deserialize)]
struct AppModeManifest {
    open_mode: String,
    entry_notebook: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    capabilities: AppModeCapabilities,
    #[serde(default)]
    skill: Option<String>,
}

/// Capability subset needed for the frontend grant prompt (additive, all
/// fields default to off so old manifests without a `capabilities` block
/// still deserialise unchanged).
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct AppModeCapabilities {
    /// When `true`, the host shows a grant prompt for active output scripts.
    #[serde(default)]
    pub active_output_scripts: bool,
    /// When `true`, the canvas capture recorder loop is enabled end-to-end.
    #[serde(default)]
    pub canvas_capture: bool,
    /// When `true`, the host injects `SPUR_ARTIFACTS_DIR` at plugin spawn.
    #[serde(default)]
    pub artifacts_dir: bool,
    /// Port capability kept as an opaque value — full parsing lives in spur-notebook.
    #[serde(default)]
    pub ports: Option<serde_json::Value>,
}

/// Richer open-mode information returned when a notebook is an app entry point.
#[derive(Debug, Serialize)]
pub struct NotebookOpenInfo {
    /// The `open_mode` declared in the manifest (e.g. `"app"`).
    pub open_mode: String,
    /// Human-readable app name.
    pub app_name: String,
    /// Absolute path to the app root directory (parent of the entry notebook).
    pub app_root: String,
    /// Declared capability flags relevant to the trust grant prompt.
    pub capabilities: AppModeCapabilities,
    /// Skill file path relative to the app root (default `"skill/SKILL.md"`).
    pub skill: String,
}

/// Get the spur-app open-mode information for a notebook when its sibling
/// manifest marks the notebook as the entry notebook.  Returns `None` when the
/// notebook has no manifest, the manifest does not match, or the manifest
/// cannot be parsed (graceful degradation, keeps notebooks opening normally).
#[tauri::command]
pub async fn notebook_open_mode(path: String) -> Result<Option<NotebookOpenInfo>, Error> {
    let notebook_path = Path::new(&path);
    let Some(dir) = notebook_path.parent() else {
        return Ok(None);
    };
    let Ok(manifest_contents) = tokio::fs::read_to_string(dir.join("spur-app.json")).await else {
        return Ok(None);
    };
    let manifest: AppModeManifest = match serde_json::from_str(&manifest_contents) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };

    if notebook_path.file_name().and_then(|name| name.to_str())
        != Some(manifest.entry_notebook.as_str())
    {
        return Ok(None);
    }

    let app_name = manifest.name.unwrap_or_else(|| "App".to_owned());
    let app_root = dir.to_string_lossy().into_owned();
    let skill = manifest
        .skill
        .unwrap_or_else(|| "skill/SKILL.md".to_owned());

    Ok(Some(NotebookOpenInfo {
        open_mode: manifest.open_mode,
        app_name,
        app_root,
        capabilities: manifest.capabilities,
        skill,
    }))
}

/// Save the contents of a Jupyter notebook to disk.
#[tauri::command]
pub async fn save_to_disk(
    path: &str,
    contents: NotebookRoot,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<(), Error> {
    info!("saving notebook at {path}");
    state
        .save_coordinator
        .save(PathBuf::from(path), contents)
        .await
}

/// Run a code cell in a Jupyter kernel.
pub async fn run_cell_events(
    notebook_path: &str,
    kernel_id: Option<&str>,
    cell_id: &str,
    state: Arc<State>,
) -> Result<async_channel::Receiver<RunCellEvent>, Error> {
    let dispatch = resolve_run_cell_dispatch(notebook_path, kernel_id, cell_id, &state)?;
    ensure_kernel_slot_live(
        &state,
        notebook_path,
        &dispatch.slot_id,
        &dispatch.spec_name,
        dispatch.code_type,
    )
    .await?;
    let conn = kernel_connection_for_slot(&state, &dispatch.slot_id)?;
    let spec_name = spec_name_for_slot(&state, &dispatch.slot_id)?;
    enforce_dispatch_spec(&dispatch.slot_id, &spec_name, dispatch.code_type)?;
    let rx = commands::run_cell_with_mode(
        &conn,
        &dispatch.wrapped_code,
        compile_progress_mode_for_spec(&spec_name),
    )
    .await?;
    Ok(track_comm_owner_for_slot(state, dispatch.slot_id, rx))
}

/// Send a Jupyter `comm_msg` to a live kernel slot over the shell channel.
pub async fn send_comm_msg(
    state: &State,
    slot_id: &str,
    comm_id: &str,
    data: serde_json::Value,
    buffers: Vec<Vec<u8>>,
) -> Result<(), Error> {
    let conn = kernel_connection_for_slot(state, slot_id)?;
    send_comm_msg_on_conn(&conn, comm_id, data, buffers).await
}

async fn send_comm_msg_on_conn(
    conn: &KernelConnection,
    comm_id: &str,
    data: serde_json::Value,
    buffers: Vec<Vec<u8>>,
) -> Result<(), Error> {
    let _pending = conn
        .call_shell(build_comm_msg(comm_id, data, buffers))
        .await?;
    Ok(())
}

fn compile_progress_mode_for_spec(spec_name: &str) -> CompileProgressMode {
    match spec_name {
        "evcxr" => CompileProgressMode::Cargo,
        "gonb" => CompileProgressMode::GoBuild,
        _ => CompileProgressMode::None,
    }
}

fn track_comm_owner_for_slot(
    state: Arc<State>,
    slot_id: String,
    rx: async_channel::Receiver<RunCellEvent>,
) -> async_channel::Receiver<RunCellEvent> {
    let (tx, tracked_rx) = async_channel::unbounded();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            update_comm_owner_for_event(&state, &slot_id, &event);
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    tracked_rx
}

fn update_comm_owner_for_event(state: &State, slot_id: &str, event: &RunCellEvent) {
    match event {
        RunCellEvent::CommOpen(open) => record_comm_open(state, slot_id, &open.comm_id),
        RunCellEvent::CommClose(close) => remove_comm_owner(state, &close.comm_id),
        _ => {}
    }
}

/// Select the per-language SPUR port bootstrap injected once per kernel session.
///
/// The bootstrap source is static; the notebook port root is bound at runtime
/// from the `SPUR_NOTEBOOK_PORT_ROOT` env the daemon sets on the kernel, so cells
/// run verbatim instead of being wrapped per dispatch.
fn bootstrap_source_for_spec(spec_name: &str) -> &'static str {
    match spec_name {
        "deno" => javascript_bootstrap(),
        "evcxr" => rust_bootstrap(),
        "gonb" => go_bootstrap(),
        _ => python_bootstrap(),
    }
}

/// Inject the SPUR port helper into a fresh kernel session.
pub async fn inject_port_bootstrap(
    conn: &crate::backend::KernelConnection,
    spec_name: &str,
) -> Result<(), Error> {
    let src = bootstrap_source_for_spec(spec_name);
    let rx = commands::run_cell(conn, src)
        .await
        .map_err(|error| Error::PortBootstrapFailed {
            stage: "dispatch",
            cause: error.to_string(),
        })?;
    drain_port_bootstrap_events(rx).await
}

async fn drain_port_bootstrap_events(
    rx: async_channel::Receiver<RunCellEvent>,
) -> Result<(), Error> {
    let mut status = None;
    let mut execute_error = None;
    let mut disconnect = None;

    while let Ok(event) = rx.recv().await {
        match event {
            RunCellEvent::Finished {
                status: finished_status,
                ..
            } => status = Some(finished_status),
            RunCellEvent::Error(error) => {
                if execute_error.is_none() {
                    execute_error = Some(format!("{}: {}", error.ename, error.evalue));
                }
            }
            RunCellEvent::Disconnect(message) => disconnect = Some(message),
            RunCellEvent::Started
            | RunCellEvent::CompileProgress { .. }
            | RunCellEvent::Stdout(_)
            | RunCellEvent::Stderr(_)
            | RunCellEvent::ExecuteResult(_)
            | RunCellEvent::DisplayData(_)
            | RunCellEvent::UpdateDisplayData(_)
            | RunCellEvent::ClearOutput(_)
            | RunCellEvent::CommOpen(_)
            | RunCellEvent::CommMsg(_)
            | RunCellEvent::CommClose(_) => {}
        }
    }

    if let Some(cause) = execute_error {
        return Err(Error::PortBootstrapFailed {
            stage: "execute-error",
            cause,
        });
    }
    if let Some(cause) = disconnect {
        return Err(Error::PortBootstrapFailed {
            stage: "disconnect",
            cause,
        });
    }
    match status.as_deref() {
        Some("ok") => Ok(()),
        Some(status) => Err(Error::PortBootstrapFailed {
            stage: "execute-reply",
            cause: format!("kernel returned status {status}"),
        }),
        None => Err(Error::PortBootstrapFailed {
            stage: "event-stream",
            cause: "kernel closed without Finished event".to_owned(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunCellDispatch {
    slot_id: String,
    spec_name: String,
    code_type: CodeType,
    wrapped_code: String,
}

fn resolve_run_cell_dispatch(
    notebook_path: &str,
    supplied_kernel_id: Option<&str>,
    cell_id: &str,
    state: &State,
) -> Result<RunCellDispatch, Error> {
    let (root, _version) = state.notebook_for_path(notebook_path).snapshot();
    let code_type = resolve_cell_code_type(&root, cell_id)?;
    let source = resolve_cell_source(&root, cell_id)?;
    let spec_name = kernelspec_for(code_type).to_owned();
    let slot_id = slot_id_for(notebook_path, code_type);

    if supplied_kernel_id.is_some_and(|kernel_id| kernel_id != slot_id) {
        info!(
            cell_id,
            ?supplied_kernel_id,
            resolved_slot_id = %slot_id,
            "classic run_cell resolved kernel slot from cell code_type"
        );
    }

    Ok(RunCellDispatch {
        slot_id,
        spec_name: spec_name.clone(),
        code_type,
        wrapped_code: source,
    })
}

fn resolve_cell_source(root: &NotebookRoot, cell_id: &str) -> Result<String, Error> {
    let cell = root
        .cells
        .iter()
        .find(|cell| notebook_cell_id(cell) == Some(cell_id))
        .ok_or_else(|| Error::NotebookDaemon(format!("cell not found: {cell_id}")))?;
    let Cell::Code(code_cell) = cell else {
        return Err(Error::NotebookDaemon(format!(
            "cell is not a code cell: {cell_id}"
        )));
    };

    Ok(code_cell.source.clone().into())
}

fn resolve_cell_code_type(root: &NotebookRoot, cell_id: &str) -> Result<CodeType, Error> {
    let cell = root
        .cells
        .iter()
        .find(|cell| notebook_cell_id(cell) == Some(cell_id))
        .ok_or_else(|| Error::NotebookDaemon(format!("cell not found: {cell_id}")))?;
    let Cell::Code(code_cell) = cell else {
        return Err(Error::NotebookDaemon(format!(
            "cell is not a code cell: {cell_id}"
        )));
    };

    Ok(code_cell
        .metadata
        .spur
        .as_ref()
        .and_then(|spur| spur.code_type)
        .or_else(|| {
            let kernelspec = root.metadata.kernelspec.as_ref()?;
            code_type_for_spec(&kernelspec.name)
        })
        .unwrap_or(CodeType::Python))
}

fn notebook_cell_id(cell: &Cell) -> Option<&str> {
    match cell {
        Cell::Raw(cell) => cell.id.as_deref(),
        Cell::Markdown(cell) => cell.id.as_deref(),
        Cell::Code(cell) => cell.id.as_deref(),
    }
}

enum KernelSlotStatus {
    Missing,
    Empty,
    Live,
}

fn kernel_slot_status(
    state: &State,
    slot_id: &str,
    spec_name: &str,
    code_type: CodeType,
) -> Result<KernelSlotStatus, Error> {
    let Some(slot) = state.kernels.get(slot_id) else {
        return Ok(KernelSlotStatus::Missing);
    };

    enforce_dispatch_spec(slot_id, slot.spec_name(), code_type)?;
    if slot.spec_name() != spec_name {
        return Err(Error::NotebookDaemon(format!(
            "refusing to run cell in slot {slot_id}: slot spec {:?} does not match resolved spec {:?}",
            slot.spec_name(),
            spec_name
        )));
    }

    if slot.kernel.is_some() {
        Ok(KernelSlotStatus::Live)
    } else {
        Ok(KernelSlotStatus::Empty)
    }
}

async fn ensure_kernel_slot_live(
    state: &Arc<State>,
    notebook_path: &str,
    slot_id: &str,
    spec_name: &str,
    code_type: CodeType,
) -> Result<(), Error> {
    if matches!(
        kernel_slot_status(state, slot_id, spec_name, code_type)?,
        KernelSlotStatus::Live
    ) {
        return Ok(());
    }

    let port_root = notebook_port_root(notebook_path);
    let working_dir = std::path::Path::new(notebook_path).parent();
    let mut kernel = start_local_kernel(spec_name, Some(&port_root), working_dir).await?;
    let status = match kernel_slot_status(state, slot_id, spec_name, code_type) {
        Ok(status) => status,
        Err(error) => {
            let _ = kernel.kill().await;
            return Err(error);
        }
    };

    match status {
        KernelSlotStatus::Live => {
            kernel.kill().await?;
            Ok(())
        }
        KernelSlotStatus::Missing | KernelSlotStatus::Empty => {
            if let Err(error) = inject_port_bootstrap(kernel.conn(), spec_name).await {
                let _ = kernel.kill().await;
                return Err(error);
            }
            let liveness = kernel.conn().liveness_token();
            install_kernel_in_slot(state, slot_id, spec_name.to_owned(), kernel);
            spawn_kernel_supervisor(state, slot_id, spec_name, liveness);
            Ok(())
        }
    }
}

fn enforce_dispatch_spec(
    slot_id: &str,
    actual_spec_name: &str,
    code_type: CodeType,
) -> Result<(), Error> {
    let expected_spec_name = kernelspec_for(code_type);
    if actual_spec_name == expected_spec_name {
        return Ok(());
    }

    Err(Error::NotebookDaemon(format!(
        "refusing to run cell in slot {slot_id}: slot spec {actual_spec_name:?} does not match code_type {code_type:?} kernelspec {expected_spec_name:?}"
    )))
}

/// Interrupt a Jupyter kernel slot.
pub async fn interrupt_kernel_slot(kernel_id: &str, state: &State) -> Result<(), Error> {
    let conn = kernel_connection_for_slot(state, kernel_id)?;
    commands::interrupt(&conn).await
}

/// Run a code cell in a Jupyter kernel.
#[tauri::command]
pub async fn run_cell(
    notebook_path: &str,
    kernel_id: Option<String>,
    cell_id: &str,
    _code: &str,
    on_event: Channel<RunCellEvent>,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<(), Error> {
    let rx = run_cell_events(
        notebook_path,
        kernel_id.as_deref(),
        cell_id,
        Arc::clone(state.inner()),
    )
    .await?;
    while let Ok(event) = rx.recv().await {
        if on_event.send(event).is_err() {
            break;
        }
    }
    Ok(())
}

/// Interrupt a running Jupyter kernel.
#[tauri::command]
pub async fn interrupt_kernel(
    kernel_id: &str,
    state: tauri::State<'_, std::sync::Arc<State>>,
) -> Result<(), Error> {
    interrupt_kernel_slot(kernel_id, &state).await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        path::PathBuf,
        sync::{
            atomic::{AtomicU32, AtomicUsize, Ordering},
            Arc, Mutex as StdMutex,
        },
    };

    use tokio::sync::{oneshot, Mutex};

    use super::*;
    use crate::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, DagSource, MultilineString,
        NotebookMetadata, PortSpec, SpurCellMetadata,
    };
    use crate::backend::wire_protocol::{
        CommMessage, CommOpen, KernelConnection, KernelMessageType,
    };
    use crate::notebook_store::DeltaKind;
    use crate::state::slot_for_comm;

    fn notebook_with_source(source: &str, version: u64) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
                metadata: CellMetadata {
                    spur: Some(SpurCellMetadata {
                        version,
                        last_edited_by: None,
                        datasource_setup: None,
                        dag: None,
                        code_type: None,
                        frontend: None,
                    }),
                    jute_deck: None,
                    other: Default::default(),
                },
                source: MultilineString::Single(source.to_string()),
                execution_count: None,
                outputs: Vec::new(),
            })],
        }
    }

    fn first_source(contents: &NotebookRoot) -> String {
        let Cell::Code(cell) = &contents.cells[0] else {
            panic!("expected code cell");
        };
        match &cell.source {
            MultilineString::Single(source) => source.clone(),
            MultilineString::Multi(lines) => lines.join(""),
        }
    }

    fn notebook_with_datasource(name: &str, path: &str) -> NotebookRoot {
        let mut notebook = notebook_with_source(&format!("spur.put('{name}', [])"), 1);
        let catalog = crate::state::DatasourceCatalog {
            schema_version: crate::state::DATASOURCE_CATALOG_SCHEMA_VERSION,
            entries: vec![DatasourceEntry {
                name: name.to_string(),
                path: path.to_string(),
                kind: DatasourceKind::Csv,
                group: None,
                columns: vec![Column {
                    name: "amount".to_string(),
                    sql_type: "DOUBLE".to_string(),
                }],
                row_count: Some(1),
                tables: Vec::new(),
            }],
        };
        catalog.persist_to_metadata(&mut notebook.metadata, None);
        notebook
    }

    fn cell_id(cell: &Cell) -> Option<&str> {
        match cell {
            Cell::Raw(cell) => cell.id.as_deref(),
            Cell::Markdown(cell) => cell.id.as_deref(),
            Cell::Code(cell) => cell.id.as_deref(),
        }
    }

    struct EnvVarGuard {
        key: String,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: String, value: &str) -> Self {
            let previous = env::var_os(&key);
            env::set_var(&key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                env::set_var(&self.key, previous);
            } else {
                env::remove_var(&self.key);
            }
        }
    }

    #[test]
    fn daemon_control_command_symbol_is_exported() {
        let _command = daemon_control;
    }

    #[tokio::test]
    async fn notebook_open_mode_entry_returns_app() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("spur-app.json"),
            r#"{"open_mode":"app","entry_notebook":"app.ipynb","name":"My App"}"#,
        )
        .expect("write manifest");
        let entry = dir.path().join("app.ipynb");
        std::fs::write(&entry, "{}").expect("write notebook");

        let result = notebook_open_mode(entry.to_string_lossy().into_owned())
            .await
            .expect("notebook_open_mode entry");
        let info = result.expect("entry notebook returns open info");
        assert_eq!(info.open_mode, "app");
        assert_eq!(info.app_name, "My App");
        assert_eq!(info.app_root, dir.path().to_string_lossy().as_ref());
        assert!(!info.capabilities.active_output_scripts);

        let other = dir.path().join("other.ipynb");
        std::fs::write(&other, "{}").expect("write other notebook");
        let other_result = notebook_open_mode(other.to_string_lossy().into_owned())
            .await
            .expect("notebook_open_mode other");
        assert!(
            other_result.is_none(),
            "non-entry notebook must return no open info"
        );
    }

    #[tokio::test]
    async fn notebook_open_mode_returns_capabilities_and_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("spur-app.json"),
            r#"{
                "open_mode": "app",
                "entry_notebook": "app.ipynb",
                "name": "Cap App",
                "capabilities": {
                    "active_output_scripts": true,
                    "canvas_capture": true
                },
                "skill": "skill/MY_SKILL.md"
            }"#,
        )
        .expect("write manifest with capabilities");
        let entry = dir.path().join("app.ipynb");
        std::fs::write(&entry, "{}").expect("write notebook");

        let info = notebook_open_mode(entry.to_string_lossy().into_owned())
            .await
            .expect("notebook_open_mode capabilities")
            .expect("entry notebook returns open info");
        assert!(info.capabilities.active_output_scripts);
        assert!(info.capabilities.canvas_capture);
        assert!(!info.capabilities.artifacts_dir);
        assert_eq!(info.skill, "skill/MY_SKILL.md");
    }

    #[tokio::test]
    async fn notebook_open_mode_no_manifest_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = dir.path().join("app.ipynb");
        std::fs::write(&entry, "{}").expect("write notebook");

        let result = notebook_open_mode(entry.to_string_lossy().into_owned())
            .await
            .expect("notebook_open_mode no manifest");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn notebook_open_mode_invalid_manifest_reports_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("spur-app.json"), r#"{invalid}"#)
            .expect("write invalid manifest");
        let entry = dir.path().join("app.ipynb");
        std::fs::write(&entry, "{}").expect("write notebook");

        let result = notebook_open_mode(entry.to_string_lossy().into_owned())
            .await
            .expect("notebook_open_mode invalid manifest");
        assert!(result.is_none(), "invalid manifest must not enter app mode");
    }

    #[tokio::test]
    async fn start_local_kernel_spawned_env_includes_notebook_port_root_and_preserves_parent_env() {
        let notebook_path = Path::new("/tmp/spur-port-root-env.ipynb");
        let port_root = notebook_port_root(notebook_path);
        let expected_root = port_root.display().to_string();
        let unique = Uuid::new_v4().to_string().replace('-', "_");
        let parent_key = format!("SPUR_NOTEBOOK_PARENT_ENV_{unique}");
        let spec_key = format!("SPUR_NOTEBOOK_SPEC_ENV_{unique}");
        let output_file = tempfile::NamedTempFile::new().expect("output temp file");
        let output_path = output_file.path().to_owned();
        let _parent_guard = EnvVarGuard::set(parent_key.clone(), "parent");

        let script = format!(
            "printf '%s|%s|%s' \"$SPUR_NOTEBOOK_PORT_ROOT\" \"${{{spec_key}}}\" \"${{{parent_key}}}\" > \"$1\""
        );
        let mut kernel_spec = environment::KernelSpec {
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                script,
                "kernel-env-test".to_string(),
                output_path.to_string_lossy().into_owned(),
            ],
            display_name: "env test".to_string(),
            language: "sh".to_string(),
            interrupt_mode: Default::default(),
            env: BTreeMap::from([(spec_key.clone(), "spec".to_string())]),
        };
        apply_notebook_port_root_env(&mut kernel_spec, Some(&port_root));

        let status = crate::backend::local::kernel_command_for_test(
            &kernel_spec.argv,
            &kernel_spec.env,
            None,
        )
        .spawn()
        .expect("spawn fake kernel env probe")
        .wait()
        .await
        .expect("wait for fake kernel env probe");
        assert!(status.success());
        let output = tokio::fs::read_to_string(&output_path)
            .await
            .expect("read env output");

        assert_eq!(
            kernel_spec
                .env
                .get("SPUR_NOTEBOOK_PORT_ROOT")
                .map(String::as_str),
            Some(expected_root.as_str())
        );
        assert_eq!(output, format!("{expected_root}|spec|parent"));
    }

    #[tokio::test]
    async fn kernel_command_runs_kernel_in_provided_working_dir() {
        let tmpdir = tempfile::tempdir().expect("create tempdir");
        let output_file = tempfile::NamedTempFile::new().expect("create temp output file");
        let output_path = output_file.path().to_owned();
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "pwd > \"$1\"".to_string(),
            "kernel-cwd-test".to_string(),
            output_path.to_string_lossy().into_owned(),
        ];
        let status = crate::backend::local::kernel_command_for_test(
            &argv,
            &BTreeMap::new(),
            Some(tmpdir.path()),
        )
        .spawn()
        .expect("spawn cwd probe")
        .wait()
        .await
        .expect("wait for cwd probe");
        assert!(status.success());
        let raw = tokio::fs::read_to_string(&output_path)
            .await
            .expect("read cwd output");
        let printed = std::path::Path::new(raw.trim());
        let expected = std::fs::canonicalize(tmpdir.path()).expect("canonicalize tmpdir");
        let actual = std::fs::canonicalize(printed).expect("canonicalize printed path");
        assert_eq!(
            actual, expected,
            "kernel cwd should be the provided working_dir"
        );
    }

    #[tokio::test]
    async fn kernel_command_none_working_dir_inherits_parent_cwd() {
        let output_file = tempfile::NamedTempFile::new().expect("create temp output file");
        let output_path = output_file.path().to_owned();
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "pwd > \"$1\"".to_string(),
            "kernel-cwd-none-test".to_string(),
            output_path.to_string_lossy().into_owned(),
        ];
        let status = crate::backend::local::kernel_command_for_test(&argv, &BTreeMap::new(), None)
            .spawn()
            .expect("spawn cwd none probe")
            .wait()
            .await
            .expect("wait for cwd none probe");
        assert!(status.success());
        let raw = tokio::fs::read_to_string(&output_path)
            .await
            .expect("read cwd none output");
        let printed = std::path::Path::new(raw.trim());
        let expected = std::fs::canonicalize(std::env::current_dir().expect("current_dir"))
            .expect("canonicalize current_dir");
        let actual = std::fs::canonicalize(printed).expect("canonicalize printed path");
        assert_eq!(
            actual, expected,
            "kernel with None working_dir should inherit parent cwd"
        );
    }

    #[test]
    fn run_cell_chokepoint_uses_stored_source() {
        let notebook_path = "/tmp/demo-notebook.ipynb";
        let state = State::new();
        let stored_source = "spur.put('sales', [1, 2])";
        state
            .get_notebook()
            .load(notebook_path, notebook_with_source(stored_source, 1));

        let dispatch = resolve_run_cell_dispatch(
            notebook_path,
            None,
            "550e8400-e29b-41d4-a716-446655440000",
            &state,
        )
        .unwrap();

        assert_eq!(dispatch.wrapped_code, stored_source);
        assert!(!dispatch.wrapped_code.contains("class _Spur"));
    }

    #[test]
    fn bootstrap_source_for_spec_routes_by_spec() {
        // Each spec maps to its language's session bootstrap; unknown specs fall
        // back to Python. The bodies read the root from env, so no spec embeds a
        // notebook path.
        assert!(bootstrap_source_for_spec("python3").contains("class _Spur"));
        assert!(bootstrap_source_for_spec("deno").contains("globalThis.spur"));
        assert!(bootstrap_source_for_spec("evcxr").contains(":dep arrow = "));
        assert!(bootstrap_source_for_spec("gonb").contains("!*go get "));
        assert!(bootstrap_source_for_spec("custom").contains("class _Spur"));
    }

    #[test]
    fn compile_progress_mode_for_spec_routes_compiled_kernels() {
        assert_eq!(
            compile_progress_mode_for_spec("evcxr"),
            crate::backend::commands::CompileProgressMode::Cargo
        );
        assert_eq!(
            compile_progress_mode_for_spec("gonb"),
            crate::backend::commands::CompileProgressMode::GoBuild
        );
        assert_eq!(
            compile_progress_mode_for_spec("python3"),
            crate::backend::commands::CompileProgressMode::None
        );
    }

    #[tokio::test]
    async fn supervisor_restarts_once_when_liveness_cancelled() {
        use tokio_util::sync::CancellationToken;

        let liveness = CancellationToken::new();
        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = calls.clone();

        let token = liveness.clone();
        let handle = tokio::spawn(async move {
            supervise_until_dead(token, || {
                calls_in.fetch_add(1, Ordering::SeqCst);
                async { Ok::<(), Error>(()) }
            })
            .await;
        });

        liveness.cancel();
        handle.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn send_comm_msg_on_conn_emits_comm_msg_on_shell_channel() {
        let (conn, shell_rx) = KernelConnection::for_test();
        let data = serde_json::json!({
            "method": "update",
            "state": { "value": 7 },
        });
        let buffers = vec![b"abc".to_vec(), vec![1, 2, 3]];

        send_comm_msg_on_conn(&conn, "comm-xyz", data.clone(), buffers.clone())
            .await
            .expect("send_comm_msg_on_conn");

        let sent = shell_rx.try_recv().expect("one shell message");
        assert!(shell_rx.try_recv().is_err(), "exactly one message");
        assert_eq!(sent.header.msg_type, KernelMessageType::CommMsg);

        let typed = sent
            .into_typed::<CommMessage>()
            .expect("comm_msg content deserializes");
        assert_eq!(typed.content.comm_id, "comm-xyz");
        assert_eq!(typed.content.data, data);
        let got: Vec<Vec<u8>> = typed.buffers.iter().map(|buffer| buffer.to_vec()).collect();
        assert_eq!(got, buffers);
    }

    #[test]
    fn run_cell_events_dispatch_routes_javascript_cell_to_deno_slot() {
        let notebook_path = "/tmp/polyglot.ipynb";
        let supplied_python_slot = format!("{}#python3", notebook_slot_id(notebook_path));
        let state = State::new();
        let mut notebook = notebook_with_source("await spur.put('sales', [])", 1);
        let Cell::Code(cell) = &mut notebook.cells[0] else {
            panic!("expected code cell");
        };
        cell.metadata.spur.as_mut().unwrap().code_type = Some(CodeType::Javascript);
        state.get_notebook().load(notebook_path, notebook);
        state.kernels.insert(
            supplied_python_slot.clone(),
            KernelSlot::new("python3".to_string()),
        );

        let dispatch = resolve_run_cell_dispatch(
            notebook_path,
            Some(&supplied_python_slot),
            "550e8400-e29b-41d4-a716-446655440000",
            &state,
        )
        .unwrap();

        assert_eq!(
            dispatch.slot_id,
            format!("{}#deno", notebook_slot_id(notebook_path))
        );
        assert_eq!(dispatch.spec_name, "deno");
        assert_eq!(dispatch.code_type, CodeType::Javascript);
        assert_eq!(dispatch.wrapped_code, "await spur.put('sales', [])");
        assert!(!dispatch.wrapped_code.contains("globalThis.spur"));
        assert!(!dispatch.wrapped_code.contains("spur = _Spur"));
    }

    #[test]
    fn run_cell_events_dispatch_routes_python_cell_to_python_slot() {
        let notebook_path = "/tmp/polyglot.ipynb";
        let supplied_python_slot = format!("{}#python3", notebook_slot_id(notebook_path));
        let state = State::new();
        let mut notebook = notebook_with_source("spur.put('sales', [1])", 1);
        let Cell::Code(cell) = &mut notebook.cells[0] else {
            panic!("expected code cell");
        };
        cell.metadata.spur.as_mut().unwrap().code_type = Some(CodeType::Python);
        state.get_notebook().load(notebook_path, notebook);
        state.kernels.insert(
            supplied_python_slot.clone(),
            KernelSlot::new("python3".to_string()),
        );

        let dispatch = resolve_run_cell_dispatch(
            notebook_path,
            Some(&supplied_python_slot),
            "550e8400-e29b-41d4-a716-446655440000",
            &state,
        )
        .unwrap();

        assert_eq!(dispatch.slot_id, supplied_python_slot);
        assert_eq!(dispatch.spec_name, "python3");
        assert_eq!(dispatch.code_type, CodeType::Python);
        assert_eq!(dispatch.wrapped_code, "spur.put('sales', [1])");
        assert!(!dispatch.wrapped_code.contains("class _Spur"));
        assert!(!dispatch.wrapped_code.contains("globalThis.spur"));
    }

    #[tokio::test]
    async fn comm_owner_updates_from_run_cell_event_stream() {
        let state = Arc::new(State::new());
        let slot_id = "slot-a".to_string();
        let (tx, rx) = async_channel::unbounded();
        let tracked_rx = track_comm_owner_for_slot(Arc::clone(&state), slot_id, rx);

        tx.send(RunCellEvent::CommOpen(CommOpen {
            comm_id: "comm-1".to_string(),
            target_name: "jupyter.widget".to_string(),
            data: serde_json::json!({}),
            buffers: Vec::new(),
        }))
        .await
        .unwrap();
        assert!(matches!(
            tracked_rx.recv().await.unwrap(),
            RunCellEvent::CommOpen(_)
        ));
        assert_eq!(slot_for_comm(&state, "comm-1").as_deref(), Some("slot-a"));

        tx.send(RunCellEvent::CommClose(CommMessage {
            comm_id: "comm-1".to_string(),
            data: serde_json::json!({}),
            buffers: Vec::new(),
        }))
        .await
        .unwrap();
        assert!(matches!(
            tracked_rx.recv().await.unwrap(),
            RunCellEvent::CommClose(_)
        ));
        assert_eq!(slot_for_comm(&state, "comm-1"), None);
    }

    #[tokio::test]
    async fn replace_notebook_hydrates_datasource_catalog_from_metadata() {
        let temp_dir = tempfile::Builder::new()
            .prefix("jute-replace-datasource-catalog-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("catalog.ipynb");
        let initial = notebook_with_datasource("sales", "/tmp/sales.csv");
        let replacement = notebook_with_datasource("inventory", "/tmp/inventory.csv");
        tokio::fs::write(&path, serde_json::to_vec(&initial).unwrap())
            .await
            .expect("seed notebook");
        let path = path.display().to_string();
        let state = State::new();

        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::LoadNotebook { path: path.clone() }),
            &state,
        )
        .await
        .into_result()
        .expect("load succeeds");
        let loaded_entries = state.datasource_catalog.lock().list();
        assert_eq!(loaded_entries[0].name, "sales");

        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::ReplaceNotebook {
                path,
                contents: replacement,
            }),
            &state,
        )
        .await
        .into_result()
        .expect("replace succeeds");

        let entries = state.datasource_catalog.lock().list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "inventory");
    }

    #[test]
    fn attach_datasource_command_round_trips_with_catalog_types() {
        let request = DaemonControlRequest::new(DaemonControlCommand::AttachDatasource {
            name: "sales".to_string(),
            path: "/tmp/sales.csv".to_string(),
            group: Some("quarterly".to_string()),
        });

        let value = serde_json::to_value(&request).expect("attach datasource serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "daemon": "notebook.v1",
                "command": "attach_datasource",
                "name": "sales",
                "path": "/tmp/sales.csv",
                "group": "quarterly"
            })
        );

        let decoded: DaemonControlRequest =
            serde_json::from_value(value).expect("attach datasource decodes");
        match decoded.command {
            DaemonControlCommand::AttachDatasource { name, path, group } => {
                assert_eq!(name, "sales");
                assert_eq!(path, "/tmp/sales.csv");
                assert_eq!(group.as_deref(), Some("quarterly"));
            }
            command => panic!("unexpected command: {command:?}"),
        }

        let entry = DatasourceEntry {
            name: "sales".to_string(),
            path: "/tmp/sales.csv".to_string(),
            kind: DatasourceKind::Csv,
            group: Some("quarterly".to_string()),
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(42),
            tables: Vec::new(),
        };
        assert_eq!(entry.kind, DatasourceKind::Csv);
        assert_eq!(entry.columns[0].sql_type, "DOUBLE");
    }

    #[test]
    fn focus_command_round_trips() {
        let request = DaemonControlRequest::new(DaemonControlCommand::SetFocus {
            notebook_id: "/tmp/notebooks/focused.ipynb".to_string(),
        });

        let value = serde_json::to_value(&request).expect("set focus serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "daemon": "notebook.v1",
                "command": "set_focus",
                "notebook_id": "/tmp/notebooks/focused.ipynb"
            })
        );

        let decoded: DaemonControlRequest =
            serde_json::from_value(value).expect("set focus decodes");
        match decoded.command {
            DaemonControlCommand::SetFocus { notebook_id } => {
                assert_eq!(notebook_id, "/tmp/notebooks/focused.ipynb");
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn cell_mutation_commands_round_trip_optional_notebook_id() {
        let without_target = serde_json::json!({
            "daemon": "notebook.v1",
            "command": "write_cell",
            "id": "cell-1",
            "source": "print(1)",
            "expected_version": 1,
            "last_edited_by": "brain"
        });
        let decoded: DaemonControlRequest =
            serde_json::from_value(without_target.clone()).expect("legacy write decodes");
        assert_eq!(
            serde_json::to_value(&decoded).expect("legacy write serializes"),
            without_target
        );

        let with_target = serde_json::json!({
            "daemon": "notebook.v1",
            "command": "write_cell",
            "notebook_id": "/tmp/notebooks/background.ipynb",
            "id": "cell-1",
            "source": "print(2)",
            "expected_version": 2,
            "last_edited_by": "brain"
        });
        let decoded: DaemonControlRequest =
            serde_json::from_value(with_target.clone()).expect("targeted write decodes");
        assert_eq!(
            serde_json::to_value(&decoded).expect("targeted write serializes"),
            with_target
        );
        match decoded.command {
            DaemonControlCommand::WriteCell { notebook_id, .. } => {
                assert_eq!(
                    notebook_id.as_deref(),
                    Some("/tmp/notebooks/background.ipynb")
                );
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[tokio::test]
    async fn focus_and_optional_notebook_id_target_store_without_changing_focus() {
        let temp_dir = tempfile::Builder::new()
            .prefix("jute-focus-target-")
            .tempdir()
            .expect("temp dir");
        let path_a = temp_dir.path().join("a.ipynb");
        let path_b = temp_dir.path().join("b.ipynb");
        std::fs::write(
            &path_a,
            serde_json::to_vec_pretty(&notebook_with_source("notebook A", 1)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &path_b,
            serde_json::to_vec_pretty(&notebook_with_source("notebook B", 1)).unwrap(),
        )
        .unwrap();
        let state = State::new();

        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::LoadNotebook {
                path: path_a.display().to_string(),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("A loads");
        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::LoadNotebook {
                path: path_b.display().to_string(),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("B loads");

        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::SetFocus {
                notebook_id: path_a.display().to_string(),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("focus A");

        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::WriteCell {
                notebook_id: Some(path_b.display().to_string()),
                id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                source: "background B edit".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("targeted write B");

        let focused = handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::Snapshot { notebook_id: None }),
            &state,
        )
        .await
        .into_result()
        .expect("focused snapshot");
        let DaemonControlResult::Snapshot(snapshot) = focused else {
            panic!("expected snapshot");
        };
        assert_eq!(first_source(&snapshot.root), "notebook A");

        let background = state.notebook_for_path(&path_b).snapshot().0;
        assert_eq!(first_source(&background), "background B edit");
    }

    #[test]
    fn attach_saved_connection_command_defaults_and_round_trips_credentials() {
        let decoded: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "attach_saved_connection",
            "name": "stripe_reporting"
        }))
        .expect("attach saved connection without credentials decodes");
        match decoded.command {
            DaemonControlCommand::AttachSavedConnection { name, credentials } => {
                assert_eq!(name, "stripe_reporting");
                assert!(credentials.is_empty());
            }
            command => panic!("unexpected command: {command:?}"),
        }

        let request = DaemonControlRequest::new(DaemonControlCommand::AttachSavedConnection {
            name: "stripe_reporting".to_string(),
            credentials: vec![("STRIPE_API_KEY".to_string(), "sk_test_123".to_string())],
        });

        let value = serde_json::to_value(&request).expect("attach saved connection serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "daemon": "notebook.v1",
                "command": "attach_saved_connection",
                "name": "stripe_reporting",
                "credentials": [["STRIPE_API_KEY", "sk_test_123"]]
            })
        );
    }

    #[test]
    fn update_saved_connection_command_round_trips() {
        let json = serde_json::json!({
            "command": "update_saved_connection",
            "name": "stripe_reporting",
            "spec_text": null,
            "credentials": [["STRIPE_API_KEY", "sk_live_x"]],
        });
        let cmd: DaemonControlCommand = serde_json::from_value(json.clone()).expect("deserializes");
        assert_eq!(serde_json::to_value(&cmd).expect("serializes"), json);
        match &cmd {
            DaemonControlCommand::UpdateSavedConnection {
                name,
                spec_text,
                credentials,
            } => {
                assert_eq!(name, "stripe_reporting");
                assert!(spec_text.is_none());
                assert_eq!(credentials.len(), 1);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        // credentials defaults to [] when omitted
        let minimal: DaemonControlCommand = serde_json::from_value(serde_json::json!({
            "command": "update_saved_connection",
            "name": "x",
            "spec_text": "openapi: 3.0.0",
        }))
        .expect("deserializes without credentials");
        assert!(matches!(
            minimal,
            DaemonControlCommand::UpdateSavedConnection { ref credentials, .. } if credentials.is_empty()
        ));
    }

    #[test]
    fn daemon_control_response_decodes_tagged_datasource_result() {
        let entry = DatasourceEntry {
            name: "sales".to_string(),
            path: "/tmp/sales.csv".to_string(),
            kind: DatasourceKind::Csv,
            group: Some("quarterly".to_string()),
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(42),
            tables: Vec::new(),
        };

        let bare_frame = serde_json::json!({
            "ok": true,
            "result": entry,
        });
        let bare_bytes = serde_json::to_vec(&bare_frame).expect("bare frame serializes");
        let bare_error = serde_json::from_slice::<DaemonControlResponse>(&bare_bytes)
            .expect_err("bare datasource result should fail strict tagged parsing");
        assert!(
            bare_error.to_string().contains("missing field `type`"),
            "unexpected bare datasource parse error: {bare_error}"
        );

        let result = serde_json::to_value(DaemonControlResult::Datasource(entry.clone()))
            .expect("datasource result serializes");
        assert_eq!(result["type"], "datasource");

        let frame = serde_json::json!({
            "ok": true,
            "result": result,
        });
        let bytes = serde_json::to_vec(&frame).expect("daemon response serializes");
        let response: DaemonControlResponse =
            serde_json::from_slice(&bytes).expect("tagged datasource response decodes");

        match response.result {
            Some(DaemonControlResult::Datasource(decoded)) => assert_eq!(decoded, entry),
            result => panic!("unexpected daemon control result: {result:?}"),
        }
    }

    #[tokio::test]
    async fn save_coordinator_collapses_queued_saves_to_latest() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let call_count = Arc::new(AtomicUsize::new(0));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (allow_first_tx, allow_first_rx) = oneshot::channel();
        let first_started_tx = Arc::new(Mutex::new(Some(first_started_tx)));
        let allow_first_rx = Arc::new(Mutex::new(Some(allow_first_rx)));

        let coordinator = SaveCoordinator::with_writer_for_test({
            let writes = Arc::clone(&writes);
            let call_count = Arc::clone(&call_count);
            let first_started_tx = Arc::clone(&first_started_tx);
            let allow_first_rx = Arc::clone(&allow_first_rx);
            move |_path: PathBuf, contents: NotebookRoot| {
                let writes = Arc::clone(&writes);
                let call_count = Arc::clone(&call_count);
                let first_started_tx = Arc::clone(&first_started_tx);
                let allow_first_rx = Arc::clone(&allow_first_rx);
                Box::pin(async move {
                    let index = call_count.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        if let Some(sender) = first_started_tx.lock().await.take() {
                            let _ = sender.send(());
                        }
                        if let Some(receiver) = allow_first_rx.lock().await.take() {
                            let _ = receiver.await;
                        }
                    }

                    writes.lock().await.push(first_source(&contents));
                    Ok(())
                })
            }
        });

        let path = PathBuf::from("notebook.ipynb");
        let first = {
            let coordinator = coordinator.clone();
            let path = path.clone();
            tokio::spawn(async move {
                coordinator
                    .save(path, notebook_with_source("first", 1))
                    .await
            })
        };

        first_started_rx.await.unwrap();
        coordinator
            .save(path.clone(), notebook_with_source("second", 2))
            .await
            .unwrap();
        coordinator
            .save(path, notebook_with_source("third", 3))
            .await
            .unwrap();
        allow_first_tx.send(()).unwrap();
        first.await.unwrap().unwrap();

        assert_eq!(
            writes.lock().await.as_slice(),
            ["first".to_string(), "third".to_string()]
        );
    }

    #[tokio::test]
    async fn atomic_write_notebook_writes_parseable_json() {
        let dir = std::env::temp_dir().join(format!("jute-save-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");

        atomic_write_notebook(&path, &notebook_with_source("persisted", 9))
            .await
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let Cell::Code(cell) = &parsed.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, 9);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn autosave_panic_before_atomic_rename_leaves_ipynb_fully_old_or_new() {
        let dir = std::env::temp_dir().join(format!("jute-save-panic-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");

        atomic_write_notebook_blocking(&path, &notebook_with_source("old", 1)).unwrap();

        let panic_result = std::panic::catch_unwind(|| {
            atomic_write_notebook_blocking_with_hook(
                &path,
                &notebook_with_source("new", 2),
                |_temp_path| panic!("simulated JS panic mid-debounce before atomic rename"),
            )
            .unwrap();
        });
        assert!(panic_result.is_err());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let source = first_source(&parsed);
        assert!(
            source == "old" || source == "new",
            "autosave target must be a complete old or new notebook, got {source:?}"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn move_notebook_to_trash_refuses_current_notebook() {
        let path = std::env::temp_dir()
            .join(format!("jute-trash-current-{}", Uuid::new_v4()))
            .join("active.ipynb");

        let result = move_notebook_to_trash_with_current_path(&path, Some(path.as_path()), |_| {
            panic!("active notebook must not be trashed");
        });

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn daemon_insert_cell_preserves_last_edited_by() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_source("initial", 1));
        let request: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "insert_cell",
            "kind": "markdown",
            "after_id": "550e8400-e29b-41d4-a716-446655440000",
            "source": "notes",
            "last_edited_by": "brain"
        }))
        .unwrap();

        let response = handle_daemon_control_request(request, &state).await;
        let result = response.into_result().unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            kind: DeltaKind::CellInserted { cell, .. },
            ..
        }) = result
        else {
            panic!("expected insert delta");
        };

        let (snapshot, _version) = state.get_notebook().snapshot();
        let inserted = snapshot
            .cells
            .iter()
            .find(|stored| cell_id(stored) == Some(cell.id.as_str()))
            .expect("inserted cell is present");
        let Cell::Markdown(cell) = inserted else {
            panic!("expected markdown cell");
        };
        assert_eq!(
            cell.metadata
                .spur
                .as_ref()
                .and_then(|spur| spur.last_edited_by.as_deref()),
            Some("brain")
        );
    }

    #[tokio::test]
    async fn daemon_set_cell_metadata_spur_dag_patch_sets_metadata() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_source("initial", 1));
        let request: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "set_cell_metadata",
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "patch": {
                "spur": {
                    "dag": {
                        "produces": [{ "port": "x", "repr": "arrow" }],
                        "consumes": [],
                        "source": { "kind": "param", "port": "p" }
                    }
                }
            },
            "expected_version": 1
        }))
        .unwrap();

        let response = handle_daemon_control_request(request, &state).await;
        let result = response.into_result().unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            kind: DeltaKind::CellWritten { cell },
            version,
            ..
        }) = result
        else {
            panic!("expected cellWritten delta");
        };
        assert_eq!(cell.id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(version, 2);

        let (snapshot, _version) = state.get_notebook().snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.metadata
                .spur
                .as_ref()
                .and_then(|spur| spur.dag.clone()),
            Some(CellDagMetadata {
                produces: vec![PortSpec {
                    port: "x".to_string(),
                    repr: "arrow".to_string(),
                    display: None,
                }],
                consumes: Vec::new(),
                source: Some(DagSource {
                    kind: "param".to_string(),
                    port: "p".to_string(),
                }),
            })
        );
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, 2);
    }

    #[tokio::test]
    async fn daemon_set_cell_metadata_spur_frontend_patch_sets_metadata() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_source("initial", 1));
        let frontend = serde_json::json!({
            "kind": "html",
            "binds": ["overview", "crates", "hotspots", "hubs", "coupling", "churn"],
            "emits": []
        });
        let request: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "set_cell_metadata",
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "patch": {
                "spur": {
                    "frontend": frontend
                }
            },
            "expected_version": 1
        }))
        .unwrap();

        let response = handle_daemon_control_request(request, &state).await;
        let result = response.into_result().unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            kind: DeltaKind::CellWritten { cell },
            version,
            ..
        }) = result
        else {
            panic!("expected cellWritten delta");
        };
        assert_eq!(cell.id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(version, 2);
        assert_eq!(
            serde_json::to_value(&cell)
                .unwrap()
                .get("frontendMetadata")
                .cloned(),
            Some(frontend.clone())
        );

        let (snapshot, _version) = state.get_notebook().snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        let spur = cell.metadata.spur.as_ref().expect("spur metadata present");
        assert_eq!(spur.version, version);
        assert_eq!(
            serde_json::to_value(spur).unwrap().get("frontend").cloned(),
            Some(frontend)
        );
    }

    #[tokio::test]
    async fn daemon_set_cell_metadata_spur_code_type_patch_sets_metadata() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_source("initial", 1));
        let request: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "set_cell_metadata",
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "patch": {
                "spur": {
                    "code_type": "rust"
                }
            },
            "expected_version": 1
        }))
        .unwrap();

        let response = handle_daemon_control_request(request, &state).await;
        let result = response.into_result().unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            kind: DeltaKind::CellWritten { cell },
            version,
            ..
        }) = result
        else {
            panic!("expected cellWritten delta");
        };
        assert_eq!(cell.id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(cell.code_type, Some(CodeType::Rust));

        let (snapshot, _version) = state.get_notebook().snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        let spur = cell.metadata.spur.as_ref().expect("spur metadata present");
        assert_eq!(spur.version, version);
        assert_eq!(spur.code_type, Some(CodeType::Rust));
    }

    #[tokio::test]
    async fn save_coordinator_preserves_authoritative_spur_dag_when_frontend_export_is_stale() {
        let dir = std::env::temp_dir().join(format!("jute-save-dag-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");
        let state = Arc::new(State::new());
        let notebook = state.get_notebook();
        notebook.load(path.clone(), notebook_with_source("initial", 1));
        notebook
            .apply(NotebookOp::SetSpurDagMetadata {
                id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                patch: CellDagMetadata {
                    produces: vec![PortSpec {
                        port: "sales".to_string(),
                        repr: "dataframe".to_string(),
                        display: Some("Sales".to_string()),
                    }],
                    consumes: vec!["config".to_string()],
                    source: Some(DagSource {
                        kind: "cell".to_string(),
                        port: "raw".to_string(),
                    }),
                },
                expected_version: 1,
            })
            .unwrap();
        notebook
            .apply(NotebookOp::MarkDatasourceSetupCell {
                id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                expected_version: 2,
            })
            .unwrap();

        let mut stale_frontend_export = notebook_with_source("frontend edit", 4);
        let Cell::Code(frontend_cell) = &mut stale_frontend_export.cells[0] else {
            panic!("expected code cell");
        };
        assert!(frontend_cell.metadata.spur.as_ref().unwrap().dag.is_none());

        state
            .save_coordinator
            .save(path.clone(), stale_frontend_export)
            .await
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        assert_eq!(first_source(&parsed), "frontend edit");
        let Cell::Code(cell) = &parsed.cells[0] else {
            panic!("expected code cell");
        };
        let spur = cell.metadata.spur.as_ref().unwrap();
        assert_eq!(spur.version, 4);
        assert_eq!(spur.datasource_setup, Some(true));
        let dag = spur.dag.as_ref().expect("save path must retain spur.dag");
        assert_eq!(dag.produces[0].port, "sales");
        assert_eq!(dag.produces[0].display.as_deref(), Some("Sales"));
        assert_eq!(dag.consumes, vec!["config"]);
        assert_eq!(dag.source.as_ref().unwrap().port, "raw");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn daemon_load_notebook_seeds_store_from_disk() {
        let dir = std::env::temp_dir().join(format!("jute-daemon-load-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loaded.ipynb");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&notebook_with_source("loaded from disk", 7)).unwrap(),
        )
        .unwrap();

        let state = Arc::new(State::new());
        let _startup_store = state.get_notebook();
        let request: DaemonControlRequest = serde_json::from_value(serde_json::json!({
            "daemon": "notebook.v1",
            "command": "load",
            "path": path
        }))
        .unwrap();

        let response = handle_daemon_control_request(request, &state).await;
        let result = response.into_result().unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            kind: DeltaKind::Loaded { .. },
            ..
        }) = result
        else {
            panic!("expected load delta");
        };

        let (snapshot, _version) = state.get_notebook().snapshot();
        assert_eq!(first_source(&snapshot), "loaded from disk");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn write_after_loading_second_notebook_does_not_mutate_second_notebook() {
        let dir = std::env::temp_dir().join(format!("jute-daemon-multi-load-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let notebook_a_path = dir.join("notebook-a.ipynb");
        let notebook_b_path = dir.join("notebook-b.ipynb");
        std::fs::write(
            &notebook_a_path,
            serde_json::to_vec_pretty(&notebook_with_source("notebook A initial", 1)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &notebook_b_path,
            serde_json::to_vec_pretty(&notebook_with_source("notebook B initial", 1)).unwrap(),
        )
        .unwrap();

        let state = Arc::new(State::new());
        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::LoadNotebook {
                path: notebook_a_path.display().to_string(),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("notebook A loads");
        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::LoadNotebook {
                path: notebook_b_path.display().to_string(),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("notebook B loads");

        state.set_focused_notebook_path(&notebook_a_path);
        handle_daemon_control_request(
            DaemonControlRequest::new(DaemonControlCommand::WriteCell {
                notebook_id: None,
                id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                source: "notebook A window edit".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("notebook-a-window".to_string()),
            }),
            &state,
        )
        .await
        .into_result()
        .expect("write from notebook A window should be accepted");

        let (snapshot, _version) = state.notebook_for_path(&notebook_b_path).snapshot();
        assert_eq!(
            first_source(&snapshot),
            "notebook B initial",
            "a write intended for notebook A must not mutate the currently loaded notebook B"
        );
        let (snapshot, _version) = state.notebook_for_path(&notebook_a_path).snapshot();
        assert_eq!(first_source(&snapshot), "notebook A window edit");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_daemon_cell_includes_inserted_last_edited_by() {
        let state = State::new();
        let store = state.get_notebook();
        store.load("/tmp/test.ipynb", notebook_with_source("initial", 1));
        let delta = store
            .apply(NotebookOp::InsertCell {
                kind: CellKind::Markdown,
                after_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
                source: "notes".to_string(),
                last_edited_by: Some("brain".to_string()),
                code_type: None,
            })
            .unwrap();
        let DeltaKind::CellInserted { cell, .. } = delta.kind else {
            panic!("expected inserted cell delta");
        };

        let (snapshot, _version) = store.snapshot();
        let reread = read_daemon_cell(&snapshot, &cell.id).unwrap();

        assert_eq!(reread.last_edited_by.as_deref(), Some("brain"));
    }

    #[tokio::test]
    async fn discard_scratch_notebooks_skips_active_notebook() {
        let scratch_dir = std::env::temp_dir().join(format!("jute-scratch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&scratch_dir).unwrap();
        let active = scratch_dir.join("active.ipynb");
        let stale = scratch_dir.join("stale.ipynb");
        let non_notebook = scratch_dir.join("note.txt");
        std::fs::write(&active, b"{}").unwrap();
        std::fs::write(&stale, b"{}").unwrap();
        std::fs::write(&non_notebook, b"not a notebook").unwrap();
        let active = normalize_path(&active).await.unwrap();
        let stale = normalize_path(&stale).await.unwrap();

        let trashed = Arc::new(StdMutex::new(Vec::<PathBuf>::new()));
        let count = discard_scratch_notebooks_in(&scratch_dir, Some(active.as_path()), {
            let trashed = Arc::clone(&trashed);
            move |path: &Path| {
                trashed.lock().unwrap().push(path.to_path_buf());
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(trashed.lock().unwrap().as_slice(), [stale]);

        std::fs::remove_dir_all(scratch_dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_control_cell_commands_round_trip_through_temp_socket() {
        use tokio::net::UnixListener;

        let dir = PathBuf::from(format!("/tmp/jute-dc-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("notebook.sock");
        let notebook_path = dir.join("notebook.ipynb");
        let cell_id = "550e8400-e29b-41d4-a716-446655440000".to_string();

        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load(&notebook_path, notebook_with_source("initial", 1));

        let listener = UnixListener::bind(&socket_path).unwrap();
        let server_state = Arc::clone(&state);
        let server = tokio::spawn(async move {
            for _ in 0..7 {
                let (mut stream, _addr) = listener.accept().await.unwrap();
                let bytes = read_daemon_frame(&mut stream).await.unwrap();
                let request: DaemonControlRequest = serde_json::from_slice(&bytes).unwrap();
                let response = handle_daemon_control_request(request, &server_state).await;
                write_daemon_frame(&mut stream, &serde_json::to_vec(&response).unwrap())
                    .await
                    .unwrap();
            }
        });

        let read = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::ReadCell {
                notebook_id: None,
                id: cell_id.clone(),
            }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        let DaemonControlResult::Cell(read) = read else {
            panic!("expected cell response");
        };
        assert_eq!(read.source, "initial");
        assert_eq!(read.version, 1);

        let write = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::WriteCell {
                notebook_id: None,
                id: cell_id.clone(),
                source: "updated".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        assert!(matches!(
            write,
            DaemonControlResult::Delta(NotebookDelta {
                version: 2,
                kind: DeltaKind::CellWritten { ref cell },
                ..
            }) if cell.id == cell_id
        ));

        let insert = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::InsertCell {
                notebook_id: None,
                kind: CellKind::Markdown,
                after_id: Some(cell_id.clone()),
                source: "notes".to_string(),
                last_edited_by: Some("brain".to_string()),
                code_type: None,
            }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        let DaemonControlResult::Delta(NotebookDelta {
            version: 3,
            kind:
                DeltaKind::CellInserted {
                    cell: inserted_cell,
                    after_id: Some(after_id),
                },
            ..
        }) = insert
        else {
            panic!("expected insert delta");
        };
        assert_eq!(after_id, cell_id);
        assert_eq!(inserted_cell.kind, "markdown");
        let inserted_id = inserted_cell.id.clone();

        let apply = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::ApplyEdit {
                notebook_id: None,
                id: inserted_id.clone(),
                source: "edited notes".to_string(),
            }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        assert!(matches!(
            apply,
            DaemonControlResult::Delta(NotebookDelta {
                version: 4,
                kind: DeltaKind::CellWritten { ref cell },
                ..
            }) if cell.id == inserted_id
        ));

        let snapshot = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::Snapshot { notebook_id: None }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        let DaemonControlResult::Snapshot(snapshot) = snapshot else {
            panic!("expected snapshot response");
        };
        assert_eq!(snapshot.version, 4);
        assert_eq!(snapshot.root.cells.len(), 2);

        let delete = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::DeleteCell {
                notebook_id: None,
                id: inserted_id.clone(),
                expected_version: 4,
            }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        assert!(matches!(
            delete,
            DaemonControlResult::Delta(NotebookDelta {
                version: 5,
                kind: DeltaKind::CellDeleted { ref id },
                ..
            }) if id == &inserted_id
        ));

        let flush = send_daemon_control_to(
            &socket_path,
            &DaemonControlRequest::new(DaemonControlCommand::FlushNotebook { notebook_id: None }),
        )
        .await
        .unwrap()
        .into_result()
        .unwrap();
        assert!(matches!(flush, DaemonControlResult::Empty {}));

        server.await.unwrap();

        let persisted: NotebookRoot =
            serde_json::from_slice(&std::fs::read(&notebook_path).unwrap()).unwrap();
        assert_eq!(first_source(&persisted), "updated");
        let Cell::Code(cell) = &persisted.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.metadata
                .spur
                .as_ref()
                .unwrap()
                .last_edited_by
                .as_deref(),
            Some("brain")
        );

        std::fs::remove_dir_all(dir).unwrap();
    }
}
