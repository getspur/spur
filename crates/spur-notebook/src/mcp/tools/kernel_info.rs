use jute::commands::kernel_slot_info_for_state;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.kernel_info";

#[derive(Debug, Deserialize)]
struct KernelInfoParams {
    kernel_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return a notebook kernel slot's status, generation, and resource usage.",
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
    let params: KernelInfoParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.kernel_info requires { kernel_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.kernel_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.kernel_info kernel_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.kernel_info requires notebook daemon state", None)
    })?;

    let info = kernel_slot_info_for_state(&params.kernel_id, state)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.kernel_info failed",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    let value = serde_json::to_value(info).map_err(|error| {
        McpError::internal_error(
            "failed to serialize notebook.kernel_info response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    Ok(CallToolResult::structured(value))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::{KernelSlot, State};

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
        }
    }

    #[tokio::test]
    async fn returns_dead_status_for_empty_slot() {
        let state = Arc::new(State::new());
        let slot_id = "notebook:/tmp/example.ipynb".to_string();
        state
            .kernels
            .insert(slot_id.clone(), KernelSlot::new("python3".into()));
        let deps = deps_with_state(state);

        let result = call(&deps, json!({ "kernel_id": slot_id }))
            .await
            .expect("kernel_info succeeds");
        let body = result.structured_content.expect("structured content");
        assert_eq!(body["kernel_id"], slot_id);
        assert_eq!(body["spec_name"], "python3");
        assert_eq!(body["status"], "dead");
    }

    #[tokio::test]
    async fn missing_slot_yields_internal_error() {
        let state = Arc::new(State::new());
        let deps = deps_with_state(state);
        let error = call(&deps, json!({ "kernel_id": "missing" }))
            .await
            .expect_err("missing slot reports error");
        assert!(error.message.contains("kernel_info"));
    }
}
