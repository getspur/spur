use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use super::parse_no_args;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook_list_datasources";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "List datasources attached to the active notebook catalog.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    parse_no_args(METHOD, arguments)?;
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook datasource catalog is not available",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let entries = state.datasource_catalog.lock().list();
    Ok(CallToolResult::structured(json!({ "entries": entries })))
}
