use jute::commands::{install_kernel_in_slot, start_local_kernel};
use jute::kernel_provision::ensure_python3_kernelspec;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.start_kernel";

#[derive(Debug, Deserialize)]
struct StartKernelParams {
    spec_name: String,
    #[serde(default)]
    slot_id: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Provision and start a Jupyter kernel in a stable slot.",
        rmcp_object(json!({
            "type": "object",
            "required": ["spec_name"],
            "properties": {
                "spec_name": { "type": "string", "minLength": 1 },
                "slot_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: StartKernelParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.start_kernel requires { spec_name, slot_id? }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.spec_name.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.start_kernel spec_name must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.start_kernel requires notebook daemon state", None)
    })?;
    let app = deps.app.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.start_kernel requires a Tauri app handle", None)
    })?;

    ensure_python3_kernelspec(app).await.map_err(|error| {
        McpError::internal_error(
            "notebook.start_kernel failed to provision kernelspec",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let kernel = start_local_kernel(&params.spec_name)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.start_kernel failed to start kernel",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    let slot_id = params
        .slot_id
        .unwrap_or_else(|| format!("mcp:{}", Uuid::new_v4()));
    let (generation, _previous) = install_kernel_in_slot(state, &slot_id, params.spec_name, kernel);

    Ok(CallToolResult::structured(json!({
        "slot_id": slot_id,
        "generation": generation,
    })))
}
