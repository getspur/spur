use jute::commands::venv::venv_create_impl;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{jute_error, require_app};
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.venv_create";

#[derive(Debug, Deserialize)]
struct VenvCreateParams {
    python_version: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Create a notebook-managed Python virtual environment.",
        rmcp_object(json!({
            "type": "object",
            "required": ["python_version"],
            "properties": {
                "python_version": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: VenvCreateParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.venv_create requires { python_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.python_version.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.venv_create python_version must not be empty",
            None,
        ));
    }

    let app = require_app(deps, METHOD)?;
    let venv_id = venv_create_impl(&params.python_version, app)
        .await
        .map_err(|error| jute_error(METHOD, &error))?;
    Ok(CallToolResult::structured(json!({ "venv_id": venv_id })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps_without_app() -> ServerDeps {
        ServerDeps::from_bridge(Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))))
    }

    #[tokio::test]
    async fn rejects_empty_python_version() {
        let deps = deps_without_app();
        let error = call(&deps, json!({ "python_version": "" }))
            .await
            .expect_err("empty version rejected");
        assert!(error.to_string().contains("python_version"));
    }

    #[tokio::test]
    async fn requires_app_handle() {
        let deps = deps_without_app();
        let error = call(&deps, json!({ "python_version": "3.12" }))
            .await
            .expect_err("missing app rejected");
        assert!(error.to_string().contains("Tauri app handle"));
    }
}
