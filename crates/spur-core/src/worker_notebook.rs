//! Notebook reads on behalf of an authenticated worker delegation.
//!
//! Only the orchestrator constructs grants. Worker arguments never choose the
//! daemon socket or widen notebook/cell scope. No active-notebook API is used.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use rmcp::{
    model::{CallToolResult, ErrorCode},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use tokio_util::sync::CancellationToken;

pub(crate) const READ_TOOLS: [&str; 2] = ["notebook_list_cells", "notebook_read_cell"];
// Match spur-notebook's existing BRIDGE_TIMEOUT; this bounds the entire IPC
// request, including connect/initialize, rather than restarting it per frame.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub(crate) struct NotebookReadGrant {
    pub(crate) path: PathBuf,
    cell_ids: Option<BTreeSet<String>>,
}

/// Explicit context is authority; neither active focus nor a worker's worktree
/// contributes paths. A missing notebook is an error, not an implicit fallback.
pub(crate) fn context_grants(
    repo_root: &Path,
    files: &[String],
) -> std::io::Result<Vec<NotebookReadGrant>> {
    let mut seen = BTreeSet::new();
    let mut grants = Vec::new();
    for file in files {
        let path = Path::new(file);
        if path.extension().and_then(|s| s.to_str()) != Some("ipynb") {
            continue;
        }
        let grant = NotebookReadGrant::new(&repo_root.join(path), None).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("invalid notebook context {file:?}: {error}"),
            )
        })?;
        if seen.insert(grant.path.clone()) {
            grants.push(grant);
        }
    }
    Ok(grants)
}

pub(crate) fn with_notebook_context(task: &str, grants: &[NotebookReadGrant]) -> String {
    if grants.is_empty() {
        return task.to_owned();
    }
    let mut prompt = String::from("## Read-only notebook context\n\nFor the notebooks below, use notebook_list_cells with the exact absolute notebook_path to discover IDs, then notebook_read_cell with notebook_path and id for effective source, outputs and cell_etag. Do not edit, execute, open, or save notebooks. These scoped reads do not change the active notebook. If unavailable or not open, report the error; do not substitute a stale disk read.\n\n");
    for grant in grants {
        prompt.push_str(&format!("- notebook_path: {}\n", json!(grant.path)));
    }
    prompt.push('\n');
    prompt.push_str(task);
    prompt
}

impl NotebookReadGrant {
    pub(crate) fn new(path: &Path, cell_ids: Option<BTreeSet<String>>) -> std::io::Result<Self> {
        let path = path.canonicalize()?;
        if path.extension().and_then(|s| s.to_str()) != Some("ipynb") || !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notebook grant requires an existing .ipynb file",
            ));
        }
        Ok(Self { path, cell_ids })
    }
}

pub(crate) struct NotebookReader {
    socket: PathBuf,
    grants: Vec<NotebookReadGrant>,
    revoked: CancellationToken,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    notebook_path: PathBuf,
    id: Option<String>,
}

impl NotebookReader {
    pub(crate) fn new(socket: PathBuf, grants: Vec<NotebookReadGrant>) -> Self {
        Self {
            socket,
            grants,
            revoked: CancellationToken::new(),
        }
    }

    pub(crate) fn revoke(&self) {
        self.revoked.cancel();
    }

    pub(crate) async fn call(&self, tool: &str, args: Value) -> Result<Value, McpError> {
        if self.revoked.is_cancelled() || !READ_TOOLS.contains(&tool) {
            return Err(not_authorized());
        }
        // Option<String> alone treats an explicit null as omission. Discovery
        // declares no id property at all, so reject its presence before parsing.
        if tool == "notebook_list_cells" && args.get("id").is_some() {
            return Err(McpError::invalid_params(
                "notebook_list_cells takes no id",
                None,
            ));
        }
        let args: ReadArgs = serde_json::from_value(args).map_err(|e| {
            McpError::invalid_params(format!("invalid notebook read arguments: {e}"), None)
        })?;
        if !args.notebook_path.is_absolute() {
            return Err(not_authorized());
        }
        let path = args
            .notebook_path
            .canonicalize()
            .map_err(|_| not_authorized())?;
        let grant = self
            .grants
            .iter()
            .find(|grant| grant.path == path)
            .ok_or_else(not_authorized)?;
        let read_cell = tool == "notebook_read_cell";
        match (read_cell, args.id.as_deref()) {
            (true, Some(id)) if !id.is_empty() => {
                if grant.cell_ids.as_ref().is_some_and(|ids| !ids.contains(id)) {
                    return Err(not_authorized());
                }
            }
            (false, None) => {}
            _ => {
                return Err(McpError::invalid_params(
                    "read_cell requires a nonempty id; list_cells takes no id",
                    None,
                ))
            }
        }
        let mut arguments = json!({"notebook_path": grant.path});
        if let Some(id) = &args.id {
            arguments["id"] = json!(id);
        }
        let result = tokio::select! {
            biased;
            _ = self.revoked.cancelled() => return Err(not_authorized()),
            result = tokio::time::timeout(READ_TIMEOUT, call_daemon(&self.socket, tool, arguments)) => {
                result.map_err(|_| McpError::internal_error("notebook read timed out", Some(json!({"code":"notebook_timeout"}))))??
            }
        };
        if self.revoked.is_cancelled() {
            return Err(not_authorized());
        }
        let result: CallToolResult = serde_json::from_value(result)
            .map_err(|_| invalid_response("invalid notebook tool result"))?;
        if result.is_error == Some(true) {
            return Err(McpError::new(
                ErrorCode(-32000),
                "notebook read failed",
                Some(
                    serde_json::to_value(result)
                        .map_err(|_| invalid_response("invalid error result"))?,
                ),
            ));
        }
        let mut body = result
            .structured_content
            .ok_or_else(|| invalid_response("notebook read has no structured result"))?;
        if body.get("path").and_then(Value::as_str).map(Path::new) != Some(grant.path.as_path())
            || body
                .get("notebook_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || body.get("revision").and_then(Value::as_u64).is_none()
        {
            return Err(invalid_response("notebook response identity mismatch"));
        }
        if read_cell {
            if body.get("id").and_then(Value::as_str) != args.id.as_deref()
                || body
                    .get("cell_etag")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(invalid_response("notebook cell response identity mismatch"));
            }
        } else {
            let rows = body
                .get("cells")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_response("notebook discovery has no cells"))?;
            let mut cells = Vec::new();
            for cell in rows {
                let id = cell
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        invalid_response("notebook discovery has invalid cell identity")
                    })?;
                let kind = cell
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_response("notebook discovery has no cell kind"))?;
                if grant.cell_ids.as_ref().is_none_or(|ids| ids.contains(id)) {
                    cells.push(json!({"id":id, "kind":kind}));
                }
            }
            // Reconstruct the discovery envelope so future upstream additions
            // cannot reveal cells or source outside a restricted cell grant.
            body = json!({"notebook_id":body["notebook_id"], "path":body["path"],
                "revision":body["revision"], "cells":cells});
        }
        Ok(body)
    }
}

pub(crate) fn not_authorized() -> McpError {
    McpError::new(
        ErrorCode(-32001),
        "worker is not authorized to read this notebook or cell",
        None,
    )
}

fn invalid_response(message: &str) -> McpError {
    McpError::internal_error(
        message.to_owned(),
        Some(json!({"code":"notebook_invalid_response"})),
    )
}

#[cfg(unix)]
async fn call_daemon(socket: &Path, tool: &str, arguments: Value) -> Result<Value, McpError> {
    use crate::orchestrator::{read_notebook_daemon_frame, write_notebook_daemon_frame};
    // Independent, bounded connections prevent replies from concurrent workers
    // being attributed to another delegation. Never spawn or open a notebook.
    let unavailable = |e: std::io::Error| {
        McpError::internal_error(
            format!("notebook daemon unavailable: {e}"),
            Some(json!({"code":"notebook_unavailable"})),
        )
    };
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(unavailable)?;
    let init = json!({"jsonrpc":"2.0", "id":0, "method":"initialize",
        "params":rmcp::model::ClientInfo::default()});
    write_notebook_daemon_frame(
        &mut stream,
        &serde_json::to_vec(&init).map_err(|_| invalid_response("initialize encoding failed"))?,
    )
    .await
    .map_err(unavailable)?;
    // The same loop handles notifications before either response. The outer
    // request timeout bounds notification-only or stalled upstream streams.
    async fn response(stream: &mut tokio::net::UnixStream, id: u64) -> Result<Value, McpError> {
        loop {
            let frame = read_notebook_daemon_frame(stream).await.map_err(|e| {
                McpError::internal_error(format!("notebook daemon unavailable: {e}"), None)
            })?;
            let value: Value = serde_json::from_slice(&frame)
                .map_err(|_| invalid_response("invalid notebook JSON-RPC frame"))?;
            if value.get("id").is_none() && value.get("method").is_some() {
                continue;
            }
            if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                || value.get("id").and_then(Value::as_u64) != Some(id)
            {
                return Err(invalid_response(
                    "notebook response request identity mismatch",
                ));
            }
            if let Some(error) = value.get("error") {
                return Err(serde_json::from_value::<McpError>(error.clone())
                    .unwrap_or_else(|_| invalid_response("invalid notebook RPC error")));
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| invalid_response("missing notebook RPC result"));
        }
    }
    response(&mut stream, 0).await?;
    let initialized = json!({"jsonrpc":"2.0", "method":"notifications/initialized"});
    write_notebook_daemon_frame(&mut stream, &serde_json::to_vec(&initialized).unwrap())
        .await
        .map_err(unavailable)?;
    let request = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":tool,"arguments":arguments}});
    write_notebook_daemon_frame(
        &mut stream,
        &serde_json::to_vec(&request)
            .map_err(|_| invalid_response("notebook request encoding failed"))?,
    )
    .await
    .map_err(unavailable)?;
    response(&mut stream, 1).await
}

#[cfg(not(unix))]
async fn call_daemon(_socket: &Path, _tool: &str, _arguments: Value) -> Result<Value, McpError> {
    Err(McpError::internal_error(
        "notebook daemon unavailable: local notebook sockets require Unix",
        None,
    ))
}

pub(crate) struct NotebookReadCatalog;

#[async_trait]
impl ToolModule for NotebookReadCatalog {
    fn tools(&self) -> Vec<ToolDefinition> {
        READ_TOOLS.into_iter().map(|name| {
            let mut schema = json!({"type":"object", "required":["notebook_path"],
                "properties":{"notebook_path":{"type":"string", "minLength":1}}, "additionalProperties":false});
            if name == "notebook_read_cell" {
                schema["required"] = json!(["notebook_path","id"]);
                schema["properties"]["id"] = json!({"type":"string","minLength":1});
            }
            ToolDefinition { name:name.into(), input_schema:schema,
                description: if name == "notebook_list_cells" {
                    "List cell IDs and kinds for an authorized task notebook. Use the absolute notebook_path from task context. Read-only; never changes active notebook."
                } else {
                    "Read effective source, outputs and cell_etag for an authorized task notebook cell. Use the absolute notebook_path and an id from notebook_list_cells. No edits or execution."
                }.into() }
        }).collect()
    }

    async fn call(
        &self,
        _ctx: ToolCallContext<'_>,
        _name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        Err(not_authorized()) // Dispatch requires the authenticated HTTP context.
    }
}

#[cfg(test)]
pub(crate) mod tests;
