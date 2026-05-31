use std::sync::Arc;

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{dag::notebook_run_context, mcp::ServerDeps};

const METHOD: &str = "notebook_run_cell";

#[derive(Debug, Deserialize)]
struct RunCellParams {
    cell_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run one notebook DAG cell and mark downstream cells stale without cascading them.",
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
    let params: RunCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_run_cell requires { cell_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_run_cell cell_id must not be empty",
            None,
        ));
    }

    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_run_cell requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_run_cell requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_run_cell requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;

    let mut context = notebook_run_context(
        &path,
        Arc::clone(state),
        Arc::clone(&deps.bridge),
        deps.app.clone(),
        deps.daemon.clone(),
    );
    let report = context
        .engine
        .run_cell(&params.cell_id)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook_run_cell failed",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    Ok(CallToolResult::structured(json!({
        "cell_id": report.cell_id,
        "status": report.status.as_str(),
    })))
}
