use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.read_cell";

#[derive(Debug, Deserialize)]
struct ReadCellParams {
    id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReadCellResult {
    id: String,
    kind: String,
    version: u64,
    source: String,
    exec_count: Option<u32>,
    status: String,
    outputs: Vec<Value>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return full source and outputs for one loaded notebook cell.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    bridge: &dyn BridgeRequester,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: ReadCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.read_cell requires { id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.read_cell id must not be empty",
            None,
        ));
    }

    let value = bridge
        .request(METHOD, json!({ "id": params.id }), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    let cell: ReadCellResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.read_cell bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(cell)))
}
