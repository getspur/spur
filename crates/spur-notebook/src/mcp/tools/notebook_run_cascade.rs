use std::sync::Arc;

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    dag::{engine::RunCellCommandRunner, notebook_port_root, ReactiveEngine},
    mcp::ServerDeps,
};

const METHOD: &str = "notebook_run_cascade";

#[derive(Debug, Deserialize)]
struct RunCascadeParams {
    cell_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run a notebook cell through the reactive DAG engine, then cascade downstream cells.",
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
    let params: RunCascadeParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_run_cascade requires { cell_id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_run_cascade cell_id must not be empty",
            None,
        ));
    }

    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_run_cascade requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_run_cascade requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_run_cascade requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;

    let runner = RunCellCommandRunner::new(Arc::new(deps.clone()));
    let mut engine = ReactiveEngine::new(
        state.get_notebook(),
        runner,
        &path,
        notebook_port_root(&path),
    );
    let report = engine
        .run_cell_and_cascade(&params.cell_id)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook_run_cascade failed",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    Ok(CallToolResult::structured(json!({
        "cell_id": params.cell_id,
        "runs": report.runs.into_iter().map(|run| {
            json!({
                "cell_id": run.cell_id,
                "status": run.status.as_str(),
            })
        }).collect::<Vec<_>>(),
    })))
}
