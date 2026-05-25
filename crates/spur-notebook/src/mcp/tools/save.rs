use std::path::PathBuf;

use jute::backend::notebook::NotebookRoot;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Emitter;

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.save";

#[derive(Debug, Deserialize)]
struct SaveParams {
    path: String,
    contents: NotebookRoot,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Persist a complete notebook document to disk.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path", "contents"],
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "contents": {
                    "type": "object",
                    "required": ["metadata", "nbformat_minor", "nbformat", "cells"],
                    "properties": {
                        "metadata": { "type": "object" },
                        "nbformat_minor": { "type": "integer", "minimum": 0 },
                        "nbformat": { "type": "integer", "minimum": 1 },
                        "cells": { "type": "array" }
                    },
                    "additionalProperties": true
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: SaveParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.save requires { path, contents }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.path.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.save path must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.save requires notebook daemon state", None)
    })?;

    state
        .save_coordinator
        .save(PathBuf::from(&params.path), params.contents)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.save failed to write notebook",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    if let Some(app) = deps.app.as_ref() {
        app.emit("notebook://saved", &json!({ "path": params.path }))
            .map_err(|error| {
                McpError::internal_error(
                    "notebook.save failed to emit saved event",
                    Some(json!({ "error": error.to_string() })),
                )
            })?;
    }

    Ok(CallToolResult::structured(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::State;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps_with_state(state: Arc<State>) -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(state),
            app: None,
            daemon: None,
        }
    }

    fn sample_notebook() -> NotebookRoot {
        serde_json::from_value(json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": [
                {
                    "cell_type": "markdown",
                    "id": "cell-1",
                    "metadata": {},
                    "source": "saved"
                }
            ]
        }))
        .expect("sample notebook parses")
    }

    #[tokio::test]
    async fn saves_notebook_to_path() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("saved.ipynb");
        let notebook = sample_notebook();
        let deps = deps_with_state(Arc::new(State::new()));

        let result = call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": notebook.clone()
            }),
        )
        .await
        .expect("save succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["ok"], true);
        let saved = tokio::fs::read_to_string(&path)
            .await
            .expect("notebook written");
        let saved_notebook: NotebookRoot =
            serde_json::from_str(&saved).expect("saved notebook parses");
        assert_eq!(saved_notebook, notebook);
    }
}
