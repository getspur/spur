use std::sync::Arc;

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    dag::{engine::EngineError, notebook_run_context},
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

    let mut context = notebook_run_context(
        &path,
        Arc::clone(state),
        Arc::clone(&deps.bridge),
        deps.app.clone(),
        deps.daemon.clone(),
    );
    let report = context
        .engine
        .run_cell_and_cascade(&params.cell_id)
        .await
        .map_err(notebook_run_cascade_error)?;

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

fn notebook_run_cascade_error(error: EngineError) -> McpError {
    match error {
        EngineError::UnsupportedKernelspec {
            spec_name,
            cell_ids,
        } => McpError::invalid_params(
            "notebook_run_cascade unsupported kernelspec",
            Some(json!({
                "code": "kernelspec_not_supported",
                "spec_name": spec_name,
                "cell_ids": cell_ids,
            })),
        ),
        error => McpError::internal_error(
            "notebook_run_cascade failed",
            Some(json!({ "error": error.to_string() })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_kernelspec_error_names_cells() {
        let error = notebook_run_cascade_error(EngineError::UnsupportedKernelspec {
            spec_name: "evcxr".to_string(),
            cell_ids: vec!["rs1".to_string(), "rs2".to_string()],
        });
        let data = error.data.expect("structured error data");

        assert_eq!(data.get("code"), Some(&json!("kernelspec_not_supported")));
        assert_eq!(data.get("cell_ids"), Some(&json!(["rs1", "rs2"])));
    }
}
