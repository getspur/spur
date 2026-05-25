use jute::commands::take_kernel_from_slot;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.stop_kernel";

#[derive(Debug, Deserialize)]
struct StopKernelParams {
    kernel_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Stop the Jupyter kernel bound to a slot and clear it.",
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
    let params: StopKernelParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.stop_kernel requires { kernel_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.kernel_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.stop_kernel kernel_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.stop_kernel requires notebook daemon state", None)
    })?;

    let mut kernel = take_kernel_from_slot(state, &params.kernel_id).map_err(|error| {
        McpError::internal_error(
            "notebook.stop_kernel failed to take kernel",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    kernel.kill().await.map_err(|error| {
        McpError::internal_error(
            "notebook.stop_kernel failed to kill kernel",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!({ "ok": true })))
}
