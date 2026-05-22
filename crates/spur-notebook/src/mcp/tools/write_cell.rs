use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.write_cell";
const LAST_EDITED_BY: &str = "brain";

#[derive(Debug, Deserialize)]
struct WriteCellParams {
    id: String,
    source: String,
    expected_version: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct WriteCellResult {
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Replace one cell's source if expected_version still matches.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id", "source", "expected_version"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "source": { "type": "string" },
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
    let params: WriteCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.write_cell requires { id, source, expected_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.write_cell id must not be empty",
            None,
        ));
    }
    if params.expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook.write_cell expected_version must be >= 1",
            None,
        ));
    }

    let value = bridge
        .request(
            METHOD,
            json!({
                "id": params.id,
                "source": params.source,
                "expected_version": params.expected_version,
                "last_edited_by": LAST_EDITED_BY
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;
    let result: WriteCellResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.write_cell bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(result)))
}
