use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook_get_cell_capture";
const BRIDGE_METHOD: &str = "notebook.get_cell_capture";

#[derive(Debug, Deserialize)]
struct CellCaptureParams {
    cell_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct CellCaptureResult {
    webm_base64: String,
    duration_sec: f64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return the latest webm video capture recorded from a text/html canvas cell.",
        rmcp_object(json!({
            "type": "object",
            "required": ["cell_id"],
            "properties": {
                "cell_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let params: CellCaptureParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ cell_id }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} cell_id must not be empty"),
            None,
        ));
    }

    let value = bridge
        .request(
            BRIDGE_METHOD,
            json!({ "cell_id": params.cell_id }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;
    let capture: CellCaptureResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            format!("invalid {BRIDGE_METHOD} bridge response"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(capture)))
}
