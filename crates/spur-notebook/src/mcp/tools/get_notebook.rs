use jute::backend::notebook::NotebookRoot;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.get_notebook";

#[derive(Debug, Deserialize)]
struct GetNotebookParams {
    path: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Read a complete notebook document from disk.",
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

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: GetNotebookParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.get_notebook requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.path.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.get_notebook path must not be empty",
            None,
        ));
    }

    let contents = tokio::fs::read_to_string(&params.path)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.get_notebook failed to read notebook",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    let notebook: NotebookRoot = serde_json::from_str(&contents).map_err(|error| {
        McpError::internal_error(
            "notebook.get_notebook failed to parse notebook",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!({ "notebook": notebook })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps() -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: None,
            app: None,
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
                    "source": "loaded"
                }
            ]
        }))
        .expect("sample notebook parses")
    }

    #[tokio::test]
    async fn returns_notebook_from_path() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-get-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("loaded.ipynb");
        let notebook = sample_notebook();
        tokio::fs::write(
            &path,
            serde_json::to_string_pretty(&notebook).expect("notebook serializes"),
        )
        .await
        .expect("notebook writes");

        let result = call(&deps(), json!({ "path": path.display().to_string() }))
            .await
            .expect("get_notebook succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(
            body["notebook"],
            serde_json::to_value(notebook).expect("notebook value")
        );
    }
}
