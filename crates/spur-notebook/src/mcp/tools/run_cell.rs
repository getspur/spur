use jute::{backend::commands::RunCellEvent, commands::run_cell_events, state::State};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Emitter;
use tracing::warn;

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.run_cell";
const RUN_CELL_EVENT_NAME: &str = "notebook://run_cell_event";

#[derive(Debug, Deserialize)]
struct RunCellParams {
    cell_id: String,
    #[serde(default)]
    kernel_id: Option<String>,
    code: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        concat!(
            "Run a code cell synchronously: the call blocks until the kernel emits Finished or ",
            "Disconnect, then returns aggregated outputs. Long-running cells will hold the MCP ",
            "request open for their full duration. kernel_id is optional when a notebook is ",
            "open; defaults to the UI-shared slot."
        ),
        rmcp_object(json!({
            "type": "object",
            "required": ["cell_id", "code"],
            "properties": {
                "cell_id": { "type": "string", "minLength": 1 },
                "kernel_id": { "type": "string" },
                "code": { "type": "string" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: RunCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.run_cell requires { cell_id, kernel_id?, code }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell cell_id must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.run_cell requires notebook daemon state", None)
    })?;
    let kernel_id = resolve_kernel_id(deps, params.kernel_id.as_deref()).await?;

    let rx = run_cell_events(&kernel_id, &params.code, state)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.run_cell failed to dispatch",
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    let summary = drain_run_cell_events(deps, state, &params.cell_id, &kernel_id, rx).await;

    Ok(CallToolResult::structured(json!({
        "id": params.cell_id,
        "status": summary.status,
        "exec_count": summary.exec_count,
        "outputs": summary.outputs,
    })))
}

async fn resolve_kernel_id(
    deps: &ServerDeps,
    explicit_kernel_id: Option<&str>,
) -> Result<String, McpError> {
    if let Some(kernel_id) = explicit_kernel_id.filter(|kernel_id| !kernel_id.is_empty()) {
        return Ok(kernel_id.to_string());
    }

    super::current_notebook_slot_id(deps).await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook.run_cell requires kernel_id when no notebook is open",
            None,
        )
    })
}

struct RunCellSummary {
    outputs: Vec<Value>,
    exec_count: Option<u32>,
    status: String,
}

async fn drain_run_cell_events(
    deps: &ServerDeps,
    state: &State,
    cell_id: &str,
    kernel_id: &str,
    rx: async_channel::Receiver<RunCellEvent>,
) -> RunCellSummary {
    let mut outputs: Vec<Value> = Vec::new();
    let mut exec_count: Option<u32> = None;
    let mut status: String = "error".to_string();

    while let Ok(event) = rx.recv().await {
        let event_value = serde_json::to_value(&event).unwrap_or(Value::Null);
        if let Some(app) = deps.app.as_ref() {
            let _ = app.emit(
                RUN_CELL_EVENT_NAME,
                json!({
                    "cell_id": cell_id,
                    "kernel_id": kernel_id,
                    "event": event_value.clone(),
                }),
            );
        }
        if let Err(error) = state.get_notebook().apply_run_event(cell_id, event.clone()) {
            warn!(
                cell_id = %cell_id,
                error = %error,
                "failed to apply run cell event to notebook store"
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
            RunCellEvent::Stdout(_)
            | RunCellEvent::Stderr(_)
            | RunCellEvent::ExecuteResult(_)
            | RunCellEvent::DisplayData(_)
            | RunCellEvent::UpdateDisplayData(_)
            | RunCellEvent::ClearOutput(_)
            | RunCellEvent::Error(_) => {
                outputs.push(event_value);
            }
        }
    }

    RunCellSummary {
        outputs,
        exec_count,
        status,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::{
        backend::notebook::{
            Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, NotebookRoot, Output,
            OutputStream,
        },
        state::State,
    };
    use serde_json::Map;

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
            daemon: None,
        }
    }

    fn notebook_with_code_cell() -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some("code-1".to_string()),
                metadata: CellMetadata {
                    spur: None,
                    other: Map::new(),
                },
                source: MultilineString::Single("print('old')".to_string()),
                execution_count: Some(1),
                outputs: vec![Output::Stream(OutputStream {
                    name: "stdout".to_string(),
                    text: MultilineString::Single("old".to_string()),
                    other: Map::new(),
                })],
            })],
        }
    }

    #[test]
    fn tool_description_discloses_synchronous_contract() {
        let tool = tool();
        let description = tool.description.expect("tool has description");

        assert!(description.contains("blocks until the kernel emits Finished or Disconnect"));
        assert!(description.contains("Long-running cells will hold the MCP request open"));
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
    async fn empty_kernel_id_requires_open_notebook() {
        let deps = deps_with_state(Arc::new(State::new()));
        let error = call(
            &deps,
            json!({ "cell_id": "code-1", "kernel_id": "", "code": "" }),
        )
        .await
        .expect_err("empty kernel_id needs a default notebook slot");
        assert!(
            error
                .message
                .contains("requires kernel_id when no notebook is open"),
            "{:?}",
            error.message
        );
    }

    #[tokio::test]
    async fn event_drain_updates_notebook_store_cell_outputs() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_code_cell());
        let deps = deps_with_state(Arc::clone(&state));
        let params = RunCellParams {
            cell_id: "code-1".to_string(),
            kernel_id: Some("kernel-1".to_string()),
            code: "print('new')".to_string(),
        };
        let (tx, rx) = async_channel::unbounded();

        tx.send(RunCellEvent::Started).await.unwrap();
        tx.send(RunCellEvent::Stdout("new".to_string()))
            .await
            .unwrap();
        tx.send(RunCellEvent::Finished {
            exec_count: Some(2),
            status: "ok".to_string(),
        })
        .await
        .unwrap();
        drop(tx);

        let summary = drain_run_cell_events(&deps, &state, &params.cell_id, "kernel-1", rx).await;

        assert_eq!(summary.status, "ok");
        assert_eq!(summary.exec_count, Some(2));
        let (snapshot, _version) = state.get_notebook().snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(cell.execution_count, Some(2));
        assert_eq!(
            cell.outputs,
            vec![Output::Stream(OutputStream {
                name: "stdout".to_string(),
                text: MultilineString::Single("new".to_string()),
                other: Map::new(),
            })]
        );
    }
}
