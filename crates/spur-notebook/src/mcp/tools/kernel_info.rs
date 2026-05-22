use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{empty_params, BRIDGE_TIMEOUT};
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.kernel_info";

#[derive(Debug, Deserialize, Serialize)]
struct KernelInfo {
    kernel_id: String,
    spec_name: String,
    generation: u64,
    status: String,
    cpu_pct: f32,
    mem_mb: f32,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return active notebook kernel slot status, generation, and usage.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call(bridge: &dyn BridgeRequester) -> Result<CallToolResult, McpError> {
    let value = bridge
        .request(METHOD, empty_params(), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    let info: KernelInfo = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.kernel_info bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(info)))
}
