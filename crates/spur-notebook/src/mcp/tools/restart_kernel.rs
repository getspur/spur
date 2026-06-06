use jute::commands::{restart_kernel_in_slot, spec_name_for_slot};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.restart_kernel";

#[derive(Debug, Deserialize)]
struct RestartKernelParams {
    slot_id: String,
    #[serde(default)]
    spec_name: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Restart the Jupyter kernel bound to an existing slot.",
        rmcp_object(json!({
            "type": "object",
            "required": ["slot_id"],
            "properties": {
                "slot_id": { "type": "string", "minLength": 1 },
                "spec_name": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: RestartKernelParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.restart_kernel requires { slot_id, spec_name? }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.slot_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.restart_kernel slot_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook.restart_kernel requires notebook daemon state",
            None,
        )
    })?;

    let spec_name = match params.spec_name {
        Some(name) => name,
        None => spec_name_for_slot(state, &params.slot_id).map_err(|error| {
            McpError::internal_error(
                "notebook.restart_kernel failed to read spec name",
                Some(json!({ "error": error.to_string() })),
            )
        })?,
    };

    let generation = restart_kernel_in_slot(state, &params.slot_id, &spec_name)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.restart_kernel failed to restart kernel",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    Ok(CallToolResult::structured(json!({
        "slot_id": params.slot_id,
        "generation": generation,
    })))
}
