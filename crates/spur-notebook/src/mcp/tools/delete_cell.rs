use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.delete_cell";

#[derive(Debug, Deserialize)]
struct DeleteCellParams {
    id: String,
    expected_version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Delete one cell if expected_version still matches.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id", "expected_version"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    bridge: &dyn BridgeRequester,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: DeleteCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.delete_cell requires { id, expected_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.delete_cell id must not be empty",
            None,
        ));
    }
    if params.expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook.delete_cell expected_version must be >= 1",
            None,
        ));
    }

    let value = bridge
        .request(
            METHOD,
            json!({
                "id": params.id,
                "expected_version": params.expected_version
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;

    Ok(CallToolResult::structured(value))
}
