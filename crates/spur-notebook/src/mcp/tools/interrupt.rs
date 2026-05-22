use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::json;

use super::{empty_params, BRIDGE_TIMEOUT};
use crate::mcp::bridge::BridgeRequester;

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

pub async fn call(bridge: &dyn BridgeRequester) -> Result<CallToolResult, McpError> {
    let value = bridge
        .request(METHOD, empty_params(), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;

    Ok(CallToolResult::structured(value))
}
