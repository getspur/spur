//! MCP tools for daemon-routed notebook lifecycle commands
//! (new/open/close/reopen). Each tool reuses the in-process
//! `NotebookDaemonControl` so we never reimplement the daemon protocol.

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{check_response, daemon_unavailable, parse_no_args};
use crate::mcp::{DaemonControlRequest, NotebookDaemonControl, ServerDeps};

fn require_daemon(deps: &ServerDeps) -> Result<&NotebookDaemonControl, McpError> {
    deps.daemon.as_ref().ok_or_else(daemon_unavailable)
}

// ---------------------------------------------------------------- notebook.new

pub fn new_tool() -> Tool {
    Tool::new(
        "notebook.new",
        "Create a new Untitled scratch notebook and open it. If a notebook is already open, the daemon first flushes its in-memory cell buffer to the current path.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call_new(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    parse_no_args("notebook.new", arguments)?;
    let daemon = require_daemon(deps)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "new".to_string(),
            path: None,
            pinned: None,
            ..Default::default()
        })
        .await;
    let response = check_response(response)?;
    let path = response.path.ok_or_else(|| {
        McpError::internal_error(
            "notebook.new daemon response missing path",
            Some(json!({ "code": "daemon_missing_path" })),
        )
    })?;
    Ok(CallToolResult::structured(json!({ "path": path })))
}

// ---------------------------------------------------------------- notebook.open

#[derive(Debug, Deserialize)]
struct OpenParams {
    path: String,
}

pub fn open_tool() -> Tool {
    Tool::new(
        "notebook.open",
        "Open a notebook at the given path through the daemon. If a notebook is already open, the daemon first flushes its in-memory cell buffer to the current path.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call_open(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: OpenParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.open requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path("notebook.open", &params.path)?;
    let fallback_path = path.to_string_lossy().into_owned();
    let daemon = require_daemon(deps)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "open".to_string(),
            path: Some(path),
            pinned: None,
            ..Default::default()
        })
        .await;
    let response = check_response(response)?;
    let path = response.path.unwrap_or(fallback_path);
    Ok(CallToolResult::structured(json!({ "path": path })))
}

// --------------------------------------------------------------- notebook.close

pub fn close_tool() -> Tool {
    Tool::new(
        "notebook.close",
        "Close the daemon's currently open notebook window after flushing its in-memory cell buffer to the current path.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call_close(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    parse_no_args("notebook.close", arguments)?;
    let daemon = require_daemon(deps)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "close".to_string(),
            path: None,
            pinned: None,
            ..Default::default()
        })
        .await;
    check_response(response)?;
    Ok(CallToolResult::structured(json!({ "ok": true })))
}

// -------------------------------------------------------------- notebook.reopen

pub fn reopen_tool() -> Tool {
    Tool::new(
        "notebook.reopen",
        "Reopen the daemon's last known notebook window and return its path.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call_reopen(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    parse_no_args("notebook.reopen", arguments)?;
    let daemon = require_daemon(deps)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "reopen".to_string(),
            path: None,
            pinned: None,
            ..Default::default()
        })
        .await;
    let response = check_response(response)?;
    let path = response.path.ok_or_else(|| {
        McpError::internal_error(
            "notebook.reopen daemon response missing path",
            Some(json!({ "code": "daemon_missing_path" })),
        )
    })?;
    Ok(CallToolResult::structured(json!({ "path": path })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps_without_daemon() -> ServerDeps {
        ServerDeps::from_bridge(Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))))
    }

    #[tokio::test]
    async fn new_surfaces_daemon_unavailable_when_unwired() {
        let deps = deps_without_daemon();
        let error = call_new(&deps, json!({}))
            .await
            .expect_err("daemon missing");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert_eq!(serialized["data"]["code"], "daemon_unavailable");
    }

    #[tokio::test]
    async fn open_rejects_missing_path() {
        let deps = deps_without_daemon();
        let error = call_open(&deps, json!({})).await.expect_err("missing path");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("path"));
    }

    #[tokio::test]
    async fn close_takes_no_arguments() {
        let deps = deps_without_daemon();
        let error = call_close(&deps, json!({ "extra": true }))
            .await
            .expect_err("rejects arguments");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("no arguments"));
    }
}
