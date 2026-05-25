use jute::backend::commands::RunCellEvent;
use jute::commands::run_cell_events;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Emitter;

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.run_cell";
const RUN_CELL_EVENT_NAME: &str = "notebook://run_cell_event";

#[derive(Debug, Deserialize)]
struct RunCellParams {
    cell_id: String,
    kernel_id: String,
    code: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run a code cell against a kernel slot, returning the full execution result.",
        rmcp_object(json!({
            "type": "object",
            "required": ["cell_id", "kernel_id", "code"],
            "properties": {
                "cell_id": { "type": "string", "minLength": 1 },
                "kernel_id": { "type": "string", "minLength": 1 },
                "code": { "type": "string" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: RunCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.run_cell requires { cell_id, kernel_id, code }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell cell_id must not be empty",
            None,
        ));
    }
    if params.kernel_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell kernel_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.run_cell requires notebook daemon state", None)
    })?;

    let rx = run_cell_events(&params.kernel_id, &params.code, state)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.run_cell failed to dispatch",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    let mut outputs: Vec<Value> = Vec::new();
    let mut exec_count: Option<u32> = None;
    let mut status: String = "error".to_string();

    while let Ok(event) = rx.recv().await {
        let event_value = serde_json::to_value(&event).unwrap_or(Value::Null);
        if let Some(app) = deps.app.as_ref() {
            let _ = app.emit(
                RUN_CELL_EVENT_NAME,
                json!({
                    "cell_id": params.cell_id,
                    "kernel_id": params.kernel_id,
                    "event": event_value,
                }),
            );
        }
        match &event {
            RunCellEvent::Finished {
                exec_count: ec,
                status: s,
            } => {
                exec_count = *ec;
                status = s.clone();
            }
            RunCellEvent::Started => {}
            RunCellEvent::Disconnect(message) => {
                status = format!("disconnect: {message}");
            }
            _ => {
                outputs.push(serde_json::to_value(&event).unwrap_or(Value::Null));
            }
        }
    }

    Ok(CallToolResult::structured(json!({
        "id": params.cell_id,
        "status": status,
        "exec_count": exec_count,
        "outputs": outputs,
    })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::State;

    use super::*;
    use crate::mcp::{
        bridge::{AgentBridge, TauriBridgeRequester},
        ServerDeps,
    };

    fn deps_with_state(state: Arc<State>) -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(state),
            app: None,
        }
    }

    #[tokio::test]
    async fn missing_slot_yields_dispatch_error() {
        let deps = deps_with_state(Arc::new(State::new()));
        let error = call(
            &deps,
            json!({ "cell_id": "code-1", "kernel_id": "missing", "code": "1+1" }),
        )
        .await
        .expect_err("missing slot reports error");
        assert!(error.message.contains("run_cell"));
    }

    #[tokio::test]
    async fn invalid_params_rejects_empty_kernel_id() {
        let deps = deps_with_state(Arc::new(State::new()));
        let error = call(
            &deps,
            json!({ "cell_id": "code-1", "kernel_id": "", "code": "" }),
        )
        .await
        .expect_err("empty kernel_id rejected");
        assert!(error.message.contains("must not be empty"));
    }
}
