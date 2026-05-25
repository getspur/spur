//! MCP tools for daemon recents — list/set_pinned/remove. List is read-only;
//! the mutating tools also fan out the recents-changed event so the
//! Tauri shell can re-render its sidebar without polling.

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{check_response, daemon_unavailable, emit_recents_changed, parse_no_args};
use crate::mcp::{DaemonControlRequest, ServerDeps};
use crate::recents;

// ---------------------------------------------------------- notebook.list_recents

pub fn list_recents_tool() -> Tool {
    Tool::new(
        "notebook.list_recents",
        "List recent notebooks recorded by the daemon, newest first.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call_list_recents(
    _deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    parse_no_args("notebook.list_recents", arguments)?;
    let entries = recents::list_recents().await.map_err(|error| {
        McpError::internal_error(
            "notebook.list_recents failed to read recents",
            Some(json!({
                "code": "recents_failed",
                "error": error.to_string()
            })),
        )
    })?;
    Ok(CallToolResult::structured(json!({ "entries": entries })))
}

// ----------------------------------------------------------- notebook.set_pinned

#[derive(Debug, Deserialize)]
struct SetPinnedParams {
    path: String,
    pinned: bool,
}

pub fn set_pinned_tool() -> Tool {
    Tool::new(
        "notebook.set_pinned",
        "Pin or unpin a notebook in the recents list.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path", "pinned"],
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "pinned": { "type": "boolean" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call_set_pinned(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: SetPinnedParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.set_pinned requires { path, pinned }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path("notebook.set_pinned", &params.path)?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "set_pinned".to_string(),
            path: Some(path),
            pinned: Some(params.pinned),
        })
        .await;
    check_response(response)?;
    emit_recents_changed(deps);
    Ok(CallToolResult::structured(json!({ "ok": true })))
}

// ------------------------------------------------ notebook.remove_from_recents

#[derive(Debug, Deserialize)]
struct RemoveParams {
    path: String,
}

pub fn remove_from_recents_tool() -> Tool {
    Tool::new(
        "notebook.remove_from_recents",
        "Forget a notebook from the recents list (does not delete the file).",
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

pub async fn call_remove_from_recents(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: RemoveParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.remove_from_recents requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path("notebook.remove_from_recents", &params.path)?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "remove_from_recents".to_string(),
            path: Some(path),
            pinned: None,
        })
        .await;
    check_response(response)?;
    emit_recents_changed(deps);
    Ok(CallToolResult::structured(json!({ "ok": true })))
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
    async fn set_pinned_surfaces_daemon_unavailable_when_unwired() {
        let deps = deps_without_daemon();
        let error = call_set_pinned(
            &deps,
            json!({ "path": "/tmp/example.ipynb", "pinned": true }),
        )
        .await
        .expect_err("daemon missing");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert_eq!(serialized["data"]["code"], "daemon_unavailable");
    }

    #[tokio::test]
    async fn remove_from_recents_rejects_missing_path() {
        let deps = deps_without_daemon();
        let error = call_remove_from_recents(&deps, json!({}))
            .await
            .expect_err("missing path");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("path"));
    }
}
