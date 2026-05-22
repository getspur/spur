use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.insert_cell";

#[derive(Debug, Deserialize)]
struct InsertCellParams {
    after_id: Option<String>,
    kind: String,
    source: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct InsertCellResult {
    id: String,
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Insert a code or markdown cell after after_id, or at the end when omitted.",
        rmcp_object(json!({
            "type": "object",
            "required": ["kind", "source"],
            "properties": {
                "after_id": { "type": "string", "minLength": 1 },
                "kind": { "type": "string", "enum": ["code", "markdown"] },
                "source": { "type": "string" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    bridge: &dyn BridgeRequester,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: InsertCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.insert_cell requires { after_id?, kind, source }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    validate_kind(&params.kind)?;
    if matches!(params.after_id.as_deref(), Some("")) {
        return Err(McpError::invalid_params(
            "notebook.insert_cell after_id must not be empty",
            None,
        ));
    }

    let mut request = json!({
        "kind": params.kind,
        "source": params.source
    });
    if let Some(after_id) = params.after_id {
        request["after_id"] = json!(after_id);
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
    if matches!(kind, "code" | "markdown") {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            "notebook.insert_cell kind must be code or markdown",
            None,
        ))
    }
}
