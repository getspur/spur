use jute::{commands::venv::venv_delete_impl, entity::EntityId};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{jute_error, require_app};
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.venv_delete";

#[derive(Debug, Deserialize)]
struct VenvDeleteParams {
    venv_id: EntityId,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Delete a notebook-managed Python virtual environment.",
        rmcp_object(json!({
            "type": "object",
            "required": ["venv_id"],
            "properties": {
                "venv_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: VenvDeleteParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.venv_delete requires { venv_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let app = require_app(deps, METHOD)?;
    let deleted = venv_delete_impl(params.venv_id, app)
        .await
        .map_err(|error| jute_error(METHOD, &error))?;
    Ok(CallToolResult::structured(json!({ "deleted": deleted })))
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
    async fn rejects_invalid_venv_id() {
        let deps = deps_without_app();
        let error = call(&deps, json!({ "venv_id": "not-a-venv" }))
            .await
            .expect_err("invalid venv id rejected");
        assert!(error.to_string().contains("venv_id"));
    }

    #[tokio::test]
    async fn requires_app_handle() {
        let deps = deps_without_app();
        let error = call(&deps, json!({ "venv_id": "ve-1234567890ab" }))
            .await
            .expect_err("missing app rejected");
        assert!(error.to_string().contains("Tauri app handle"));
    }
}
