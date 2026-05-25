use jute::commands::venv::venv_list_python_versions_impl;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use super::{jute_error, parse_no_args, require_app};
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.venv_list_python_versions";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "List managed CPython versions available for notebook virtual environments.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    parse_no_args(METHOD, arguments)?;
    let app = require_app(deps, METHOD)?;
    let versions = venv_list_python_versions_impl(app)
        .await
        .map_err(|error| jute_error(METHOD, &error))?;
    Ok(CallToolResult::structured(json!(versions)))
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
    async fn rejects_arguments() {
        let deps = deps_without_app();
        let error = call(&deps, json!({ "extra": true }))
            .await
            .expect_err("arguments rejected");
        assert!(error.to_string().contains("no arguments"));
    }

    #[tokio::test]
    async fn requires_app_handle() {
        let deps = deps_without_app();
        let error = call(&deps, json!({}))
            .await
            .expect_err("missing app rejected");
        assert!(error.to_string().contains("Tauri app handle"));
    }
}
