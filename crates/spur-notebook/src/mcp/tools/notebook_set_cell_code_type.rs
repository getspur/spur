use jute::backend::notebook::CodeType;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook_set_cell_code_type";
const BRIDGE_METHOD: &str = "notebook.set_cell_metadata";
const LAST_EDITED_BY: &str = "brain";

#[derive(Debug, Deserialize)]
struct SetCellCodeTypeParams {
    id: String,
    code_type: CodeType,
    expected_version: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SetCellCodeTypeResult {
    ok: bool,
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Set the per-cell code language metadata on one code cell. Requires \
         expected_version for optimistic concurrency.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id", "code_type", "expected_version"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "code_type": { "type": "string", "enum": ["python", "javascript", "rust"] },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let params: SetCellCodeTypeParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_set_cell_code_type requires { id, code_type, expected_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_set_cell_code_type id must not be empty",
            None,
        ));
    }
    if params.expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook_set_cell_code_type expected_version must be >= 1",
            None,
        ));
    }

    let value = bridge
        .request(
            BRIDGE_METHOD,
            json!({
                "id": params.id,
                "patch": {
                    "spur": {
                        "code_type": params.code_type
                    }
                },
                "expected_version": params.expected_version,
                "last_edited_by": LAST_EDITED_BY
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;
    let result: SetCellCodeTypeResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.set_cell_metadata bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(result)))
}
