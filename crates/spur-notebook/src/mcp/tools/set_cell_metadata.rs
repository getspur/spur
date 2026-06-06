use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.set_cell_metadata";

#[derive(Debug, Deserialize)]
struct SetCellMetadataParams {
    id: String,
    patch: Value,
    expected_version: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SetCellMetadataResult {
    ok: bool,
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Atomically update supported cell metadata patches: cell.metadata.jute_deck \
         plus cell.metadata.spur datasource_setup, dag, code_type, and frontend. \
         Requires expected_version and follows the same optimistic-concurrency protocol as write_cell.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id", "patch", "expected_version"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "patch": { "type": "object" },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let params: SetCellMetadataParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.set_cell_metadata requires { id, patch, expected_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.set_cell_metadata id must not be empty",
            None,
        ));
    }
    if params.expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook.set_cell_metadata expected_version must be >= 1",
            None,
        ));
    }

    let value = bridge
        .request(
            METHOD,
            json!({
                "id": params.id,
                "patch": params.patch,
                "expected_version": params.expected_version
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;
    let result: SetCellMetadataResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.set_cell_metadata bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(result)))
}
