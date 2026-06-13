use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use super::parse_no_args;
use crate::{context::pack::build_context_pack, mcp::ServerDeps};

const METHOD: &str = "notebook_context_pack";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Orientation pack for the active notebook: app identity + skill, cell/language summary, datasource catalog summary, DAG health. Call this FIRST when answering from notebook state, then follow next_queries.",
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
            "notebook_context_pack requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_context_pack requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_context_pack requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;
    let entries = state.datasource_catalog.lock().list();
    Ok(CallToolResult::structured(build_context_pack(
        state, &path, &entries,
    )))
}
