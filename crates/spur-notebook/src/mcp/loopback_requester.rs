use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use jute::{
    backend::notebook::{Cell, MultilineString, NotebookRoot, Output},
    commands::{DaemonControlCommand, DaemonControlRequest},
    notebook_store::CellKind,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::bridge::{BridgeError, BridgeRequestFuture, BridgeRequester};

/// Same-process bridge that loops back through the Unix socket.
/// This reuses the `handle_daemon_connection` dispatch path.
/// For MCP transport it is functionally equivalent to the Tauri bridge.
/// It skips the JavaScript round-trip used by the Tauri bridge.
#[derive(Debug)]
pub struct LoopbackDaemonRequester {
    socket_path: PathBuf,
    closed: AtomicBool,
}

impl LoopbackDaemonRequester {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            closed: AtomicBool::new(false),
        }
    }

    async fn request_inner(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<Value, BridgeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BridgeError::AppRestarted);
        }

        let request = DaemonControlRequest::new(command_from_bridge_method(method, params)?);
        let result = send_daemon_request(&self.socket_path, &request).await?;
        bridge_response_from_daemon_result(method, result)
    }
}

impl BridgeRequester for LoopbackDaemonRequester {
    fn listener_registered(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn window_alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn notebook_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn request<'a>(
        &'a self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        Box::pin(async move {
            tokio::time::timeout(timeout, self.request_inner(method, params))
                .await
                .map_err(|_| BridgeError::Timeout)?
        })
    }

    fn flush_pending<'a>(&'a self, timeout: Duration) -> BridgeRequestFuture<'a> {
        self.request("notebook.flush_pending", json!({}), timeout)
    }

    fn drain_on_shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.closed.store(true, Ordering::SeqCst);
        })
    }
}

#[derive(Debug, Deserialize)]
struct WriteCellParams {
    id: String,
    source: String,
    #[serde(alias = "expectedVersion")]
    expected_version: u64,
    #[serde(default, alias = "lastEditedBy")]
    last_edited_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadCellParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct InsertCellParams {
    kind: String,
    #[serde(default, alias = "afterId")]
    after_id: Option<String>,
    source: String,
    #[serde(default, alias = "lastEditedBy")]
    last_edited_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoadNotebookParams {
    path: String,
}

#[derive(Debug, Deserialize)]
struct DeleteCellParams {
    id: String,
    #[serde(alias = "expectedVersion")]
    expected_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonSocketResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<DaemonSocketError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonSocketError {
    code: String,
    message: String,
}

fn command_from_bridge_method(
    method: &'static str,
    params: Value,
) -> Result<DaemonControlCommand, BridgeError> {
    match method {
        "notebook.write_cell" => {
            let params: WriteCellParams = decode_params(method, params)?;
            if params.id.is_empty() {
                return Err(invalid_params("notebook.write_cell id must not be empty"));
            }
            if params.expected_version == 0 {
                return Err(invalid_params(
                    "notebook.write_cell expected_version must be >= 1",
                ));
            }
            Ok(DaemonControlCommand::WriteCell {
                id: params.id,
                source: params.source,
                expected_version: Some(params.expected_version),
                last_edited_by: params.last_edited_by,
            })
        }
        "notebook.read_cell" => {
            let params: ReadCellParams = decode_params(method, params)?;
            if params.id.is_empty() {
                return Err(invalid_params("notebook.read_cell id must not be empty"));
            }
            Ok(DaemonControlCommand::ReadCell { id: params.id })
        }
        "notebook.insert_cell" => {
            let params: InsertCellParams = decode_params(method, params)?;
            if matches!(params.after_id.as_deref(), Some("")) {
                return Err(invalid_params(
                    "notebook.insert_cell after_id must not be empty",
                ));
            }
            Ok(DaemonControlCommand::InsertCell {
                kind: bridge_cell_kind(&params.kind)?,
                after_id: params.after_id,
                source: params.source,
                last_edited_by: params.last_edited_by,
            })
        }
        "notebook.load" => {
            let params: LoadNotebookParams = decode_params(method, params)?;
            if params.path.is_empty() {
                return Err(invalid_params("notebook.load path must not be empty"));
            }
            Ok(DaemonControlCommand::LoadNotebook { path: params.path })
        }
        "notebook.delete_cell" => {
            let params: DeleteCellParams = decode_params(method, params)?;
            if params.id.is_empty() {
                return Err(invalid_params("notebook.delete_cell id must not be empty"));
            }
            if params.expected_version == 0 {
                return Err(invalid_params(
                    "notebook.delete_cell expected_version must be >= 1",
                ));
            }
            Ok(DaemonControlCommand::DeleteCell {
                id: params.id,
                expected_version: params.expected_version,
            })
        }
        "notebook.snapshot" => Ok(DaemonControlCommand::Snapshot {}),
        "notebook.flush" | "notebook.flush_pending" | "notebook.flush_notebook" => {
            Ok(DaemonControlCommand::FlushNotebook {})
        }
        method => Err(BridgeError::Handler {
            code: "unknown_method".to_string(),
            message: format!("Unknown notebook agent method: {method}"),
        }),
    }
}

fn decode_params<T>(method: &'static str, params: Value) -> Result<T, BridgeError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(params).map_err(|error| BridgeError::Handler {
        code: "invalid_params".to_string(),
        message: format!("{method} received invalid params: {error}"),
    })
}

fn bridge_cell_kind(kind: &str) -> Result<CellKind, BridgeError> {
    match kind {
        "code" => Ok(CellKind::Code),
        "markdown" => Ok(CellKind::Markdown),
        _ => Err(invalid_params(
            "notebook.insert_cell kind must be code or markdown",
        )),
    }
}

#[cfg(unix)]
async fn send_daemon_request(
    socket_path: &Path,
    request: &DaemonControlRequest,
) -> Result<Value, BridgeError> {
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(socket_error)?;
    let bytes = serde_json::to_vec(request).map_err(protocol_error)?;
    jute::commands::write_daemon_frame(&mut stream, &bytes)
        .await
        .map_err(daemon_error)?;
    let bytes = jute::commands::read_daemon_frame(&mut stream)
        .await
        .map_err(daemon_error)?;
    let response: DaemonSocketResponse = serde_json::from_slice(&bytes).map_err(protocol_error)?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        let error = response.error.unwrap_or_else(|| DaemonSocketError {
            code: "daemon_command_failed".to_string(),
            message: "daemon command failed without an error body".to_string(),
        });
        Err(BridgeError::Handler {
            code: error.code,
            message: error.message,
        })
    }
}

#[cfg(not(unix))]
async fn send_daemon_request(
    _socket_path: &Path,
    _request: &DaemonControlRequest,
) -> Result<Value, BridgeError> {
    Err(BridgeError::Handler {
        code: "unsupported_platform".to_string(),
        message: "notebook daemon socket commands are only available on Unix platforms".to_string(),
    })
}

fn bridge_response_from_daemon_result(
    method: &'static str,
    result: Value,
) -> Result<Value, BridgeError> {
    match method {
        "notebook.write_cell" => {
            delta_version(result, "cellWritten").map(|version| json!({ "version": version }))
        }
        "notebook.insert_cell" => inserted_cell_result(result),
        "notebook.delete_cell" => {
            let _ = delta_version(result, "cellDeleted")?;
            Ok(json!({ "deleted": true }))
        }
        "notebook.read_cell" => daemon_cell_to_bridge_value(result),
        "notebook.snapshot" => daemon_snapshot_to_bridge_value(result),
        "notebook.load" => {
            let _ = delta_version(result, "loaded")?;
            Ok(Value::Null)
        }
        "notebook.flush" | "notebook.flush_pending" | "notebook.flush_notebook" => Ok(Value::Null),
        method => Err(BridgeError::Handler {
            code: "unknown_method".to_string(),
            message: format!("Unknown notebook agent method: {method}"),
        }),
    }
}

fn delta_version(result: Value, expected_kind: &str) -> Result<u64, BridgeError> {
    let data = result_data(&result, "delta")?;
    let kind = data
        .get("kind")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_daemon_result("delta result did not include kind"))?;
    let actual_kind = kind
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_daemon_result("delta kind did not include type"))?;
    if actual_kind != expected_kind {
        return Err(invalid_daemon_result(format!(
            "expected delta kind {expected_kind}, got {actual_kind}"
        )));
    }
    data.get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_daemon_result("delta result did not include version"))
}

fn inserted_cell_result(result: Value) -> Result<Value, BridgeError> {
    let data = result_data(&result, "delta")?;
    let version = data
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_daemon_result("insert delta did not include version"))?;
    let kind = data
        .get("kind")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_daemon_result("insert delta did not include kind"))?;
    let actual_kind = kind
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_daemon_result("insert delta kind did not include type"))?;
    if actual_kind != "cellInserted" {
        return Err(invalid_daemon_result(format!(
            "expected delta kind cellInserted, got {actual_kind}"
        )));
    }
    let id = kind
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_daemon_result("insert delta did not include id"))?;
    Ok(json!({ "id": id, "version": version }))
}

fn daemon_cell_to_bridge_value(result: Value) -> Result<Value, BridgeError> {
    let data = result_data(&result, "cell")?;
    bridge_cell_value_from_daemon_data(data.clone())
}

fn daemon_snapshot_to_bridge_value(result: Value) -> Result<Value, BridgeError> {
    let data = result_data(&result, "snapshot")?;
    let root: NotebookRoot = serde_json::from_value(
        data.get("root")
            .cloned()
            .ok_or_else(|| invalid_daemon_result("snapshot result did not include root"))?,
    )
    .map_err(protocol_error)?;

    root.cells
        .iter()
        .map(snapshot_cell_to_bridge_value)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn result_data<'a>(result: &'a Value, expected_type: &str) -> Result<&'a Value, BridgeError> {
    let actual_type = result
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_daemon_result("daemon result did not include type"))?;
    if actual_type != expected_type {
        return Err(invalid_daemon_result(format!(
            "expected daemon result type {expected_type}, got {actual_type}"
        )));
    }
    result
        .get("data")
        .ok_or_else(|| invalid_daemon_result("daemon result did not include data"))
}

fn bridge_cell_value_from_daemon_data(mut data: Value) -> Result<Value, BridgeError> {
    let object = data
        .as_object_mut()
        .ok_or_else(|| invalid_daemon_result("cell result data was not an object"))?;
    if let Some(exec_count) = object.remove("execCount") {
        object.insert("exec_count".to_string(), exec_count);
    } else {
        object
            .entry("exec_count".to_string())
            .or_insert(Value::Null);
    }
    Ok(data)
}

fn snapshot_cell_to_bridge_value(cell: &Cell) -> Result<Value, BridgeError> {
    let (id, kind, version, source, exec_count, outputs) = match cell {
        Cell::Raw(cell) => (
            cell.id.clone(),
            "raw",
            cell.metadata
                .spur
                .as_ref()
                .map(|metadata| metadata.version)
                .unwrap_or_default(),
            multiline_to_string(&cell.source),
            None,
            Vec::new(),
        ),
        Cell::Markdown(cell) => (
            cell.id.clone(),
            "markdown",
            cell.metadata
                .spur
                .as_ref()
                .map(|metadata| metadata.version)
                .unwrap_or_default(),
            multiline_to_string(&cell.source),
            None,
            Vec::new(),
        ),
        Cell::Code(cell) => (
            cell.id.clone(),
            "code",
            cell.metadata
                .spur
                .as_ref()
                .map(|metadata| metadata.version)
                .unwrap_or_default(),
            multiline_to_string(&cell.source),
            cell.execution_count,
            cell.outputs
                .iter()
                .map(output_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };

    Ok(json!({
        "id": id.unwrap_or_default(),
        "kind": kind,
        "version": version,
        "exec_count": exec_count,
        "status": "idle",
        "source": source,
        "outputs": outputs
    }))
}

fn output_to_value(output: &Output) -> Result<Value, BridgeError> {
    serde_json::to_value(output).map_err(protocol_error)
}

fn multiline_to_string(source: &MultilineString) -> String {
    match source {
        MultilineString::Single(source) => source.clone(),
        MultilineString::Multi(lines) if lines.len() == 1 => lines[0].clone(),
        MultilineString::Multi(lines) => lines.join(""),
    }
}

fn invalid_params(message: impl Into<String>) -> BridgeError {
    BridgeError::Handler {
        code: "invalid_params".to_string(),
        message: message.into(),
    }
}

fn invalid_daemon_result(message: impl Into<String>) -> BridgeError {
    BridgeError::Handler {
        code: "invalid_daemon_result".to_string(),
        message: message.into(),
    }
}

fn protocol_error(error: impl std::error::Error) -> BridgeError {
    BridgeError::Handler {
        code: "daemon_protocol_error".to_string(),
        message: error.to_string(),
    }
}

#[cfg(unix)]
fn socket_error(error: std::io::Error) -> BridgeError {
    BridgeError::Handler {
        code: "daemon_socket_error".to_string(),
        message: error.to_string(),
    }
}

#[cfg(unix)]
fn daemon_error(error: jute::Error) -> BridgeError {
    BridgeError::Handler {
        code: "daemon_socket_error".to_string(),
        message: error.to_string(),
    }
}
