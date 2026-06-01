use jute::backend::notebook::CodeType;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.insert_cell";
const LAST_EDITED_BY: &str = "brain";

#[derive(Debug, Deserialize)]
struct InsertCellParams {
    after_id: Option<String>,
    kind: String,
    source: String,
    code_type: Option<CodeType>,
}

#[derive(Debug, Deserialize, Serialize)]
struct InsertCellResult {
    id: String,
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Insert a code, markdown, or raw cell after after_id, or at the end when omitted.",
        rmcp_object(json!({
            "type": "object",
            "required": ["kind", "source"],
            "properties": {
                "after_id": { "type": "string", "minLength": 1 },
                "kind": { "type": "string", "enum": ["code", "markdown", "raw"] },
                "source": { "type": "string" },
                "code_type": { "type": "string", "enum": ["python", "javascript", "rust"] }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let params: InsertCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.insert_cell requires { after_id?, kind, source, code_type? }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    validate_kind(&params.kind)?;
    validate_code_type(&params.kind, params.code_type)?;
    if matches!(params.after_id.as_deref(), Some("")) {
        return Err(McpError::invalid_params(
            "notebook.insert_cell after_id must not be empty",
            None,
        ));
    }

    let mut request = json!({
        "kind": params.kind,
        "source": params.source,
        "last_edited_by": LAST_EDITED_BY
    });
    if let Some(after_id) = params.after_id {
        request["after_id"] = json!(after_id);
    }
    if let Some(code_type) = params.code_type {
        request["code_type"] = json!(code_type);
    }

    let value = bridge
        .request(METHOD, request, BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    let result: InsertCellResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.insert_cell bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(result)))
}

fn validate_kind(kind: &str) -> Result<(), McpError> {
    if matches!(kind, "code" | "markdown" | "raw") {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            "notebook.insert_cell kind must be code, markdown, or raw",
            None,
        ))
    }
}

fn validate_code_type(kind: &str, code_type: Option<CodeType>) -> Result<(), McpError> {
    match (kind, code_type) {
        ("code", Some(_)) => Ok(()),
        ("code", None) => Err(McpError::invalid_params(
            "code_type required for code cells",
            None,
        )),
        ("markdown" | "raw", None) => Ok(()),
        ("markdown" | "raw", Some(_)) => Err(McpError::invalid_params(
            "code_type must be absent for non-code cells",
            None,
        )),
        _ => Ok(()),
    }
}
