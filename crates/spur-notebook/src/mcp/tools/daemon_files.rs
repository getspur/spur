//! MCP tools that wrap the daemon's filesystem-side helpers in jute::commands.
//! These do not pass through the daemon control plane — they reuse the public
//! Tauri-command surface so we never duplicate the trash/reveal/scratch logic.

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{emit_recents_changed, parse_no_args};
use crate::mcp::ServerDeps;

fn jute_error(code: &str, message: &str, error: &jute::Error) -> McpError {
    McpError::internal_error(
        message.to_string(),
        Some(json!({
            "code": code,
            "error": error.to_string()
        })),
    )
}

// ----------------------------------------------------------- notebook.move_to_trash

#[derive(Debug, Deserialize)]
struct PathParams {
    path: String,
}

pub fn move_to_trash_tool() -> Tool {
    Tool::new(
        "notebook.move_to_trash",
        "Move a notebook file to the OS trash. Refuses to trash the currently loaded notebook.",
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

pub async fn call_move_to_trash(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: PathParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.move_to_trash requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path("notebook.move_to_trash", &params.path)?;
    jute::commands::move_notebook_to_trash(path.to_string_lossy().into_owned())
        .await
        .map_err(|error| jute_error("trash_failed", "notebook.move_to_trash failed", &error))?;
    emit_recents_changed(deps);
    Ok(CallToolResult::structured(json!({ "ok": true })))
}

// ------------------------------------------------------ notebook.reveal_in_finder

pub fn reveal_in_finder_tool() -> Tool {
    Tool::new(
        "notebook.reveal_in_finder",
        "Reveal a notebook in the platform file manager.",
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

pub async fn call_reveal_in_finder(
    _deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: PathParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.reveal_in_finder requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path("notebook.reveal_in_finder", &params.path)?;
    jute::commands::reveal_notebook_in_finder(path.to_string_lossy().into_owned())
        .await
        .map_err(|error| jute_error("reveal_failed", "notebook.reveal_in_finder failed", &error))?;
    Ok(CallToolResult::structured(json!({ "ok": true })))
}

// ------------------------------------------------------ notebook.discard_scratch

pub fn discard_scratch_tool() -> Tool {
    Tool::new(
        "notebook.discard_scratch",
        "Trash all inactive scratch notebooks. Returns the count trashed.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call_discard_scratch(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    parse_no_args("notebook.discard_scratch", arguments)?;
    let count = jute::commands::discard_scratch_notebooks()
        .await
        .map_err(|error| {
            jute_error(
                "discard_scratch_failed",
                "notebook.discard_scratch failed",
                &error,
            )
        })?;
    emit_recents_changed(deps);
    Ok(CallToolResult::structured(json!({ "count": count })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps() -> ServerDeps {
        ServerDeps::from_bridge(Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))))
    }

    #[tokio::test]
    async fn move_to_trash_rejects_empty_path() {
        let deps = deps();
        let error = call_move_to_trash(&deps, json!({ "path": "" }))
            .await
            .expect_err("empty path rejected");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("path"));
    }

    #[tokio::test]
    async fn reveal_in_finder_requires_path_argument() {
        let deps = deps();
        let error = call_reveal_in_finder(&deps, json!({}))
            .await
            .expect_err("missing path rejected");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("path"));
    }

    #[tokio::test]
    async fn discard_scratch_takes_no_arguments() {
        let deps = deps();
        let error = call_discard_scratch(&deps, json!({ "extra": 1 }))
            .await
            .expect_err("rejects arguments");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert!(serialized["message"]
            .as_str()
            .unwrap_or("")
            .contains("no arguments"));
    }
}
