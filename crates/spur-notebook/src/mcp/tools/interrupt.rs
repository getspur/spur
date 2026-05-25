use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::json;

use super::{empty_params, BRIDGE_TIMEOUT};
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.interrupt";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Interrupt the active notebook kernel slot.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let value = bridge
        .request(METHOD, empty_params(), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;

    Ok(CallToolResult::structured(value))
}
