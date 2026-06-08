use jute::commands::interrupt_kernel_slot;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.interrupt";

#[derive(Debug, Deserialize)]
struct InterruptParams {
    kernel_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Interrupt a Jupyter kernel running in the given notebook slot.",
        rmcp_object(json!({
            "type": "object",
            "required": ["kernel_id"],
            "properties": {
                "kernel_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: InterruptParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.interrupt requires { kernel_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.kernel_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.interrupt kernel_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.interrupt requires notebook daemon state", None)
    })?;

    interrupt_kernel_slot(&params.kernel_id, state)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.interrupt failed",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    Ok(CallToolResult::structured(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::State;

    use super::*;
    use crate::mcp::{
        bridge::{AgentBridge, TauriBridgeRequester},
        ServerDeps,
    };

    fn deps_with_state(state: Arc<State>) -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(state),
            app: None,
            daemon: None,
            plugins: None,
        }
    }

    #[tokio::test]
    async fn missing_slot_yields_internal_error() {
        let deps = deps_with_state(Arc::new(State::new()));
        let error = call(&deps, json!({ "kernel_id": "missing" }))
            .await
            .expect_err("missing slot reports error");
        assert!(error.message.contains("interrupt"));
    }

    #[tokio::test]
    async fn invalid_params_rejects_empty_kernel_id() {
        let deps = deps_with_state(Arc::new(State::new()));
        let error = call(&deps, json!({ "kernel_id": "" }))
            .await
            .expect_err("empty kernel_id rejected");
        assert!(error.message.contains("must not be empty"));
    }
}
