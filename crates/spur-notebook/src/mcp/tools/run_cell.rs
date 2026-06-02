use jute::{
    backend::{commands::RunCellEvent, notebook::Cell},
    commands::run_cell_events,
    state::State,
};
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
    notebook_path: String,
    #[serde(default)]
    kernel_id: Option<String>,
    code: String,
    #[serde(default)]
    expected_version: Option<u64>,
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
            "required": ["cell_id", "notebook_path", "code"],
            "properties": {
                "cell_id": { "type": "string", "minLength": 1 },
                "notebook_path": { "type": "string", "minLength": 1 },
                "kernel_id": { "type": "string" },
                "code": { "type": "string" },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: RunCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.run_cell requires { cell_id, notebook_path, kernel_id?, code }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell cell_id must not be empty",
            None,
        ));
    }
    if params.notebook_path.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell notebook_path must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.run_cell requires notebook daemon state", None)
    })?;
    if let Some(expected_version) = params.expected_version {
        ensure_expected_version(state, &params.cell_id, expected_version)?;
    }
    ensure_code_cell(state, &params.cell_id)?;
    let kernel_id = resolve_kernel_id(deps, params.kernel_id.as_deref()).await?;

    let rx = run_cell_events(
        &params.notebook_path,
        Some(&kernel_id),
        &params.cell_id,
        &params.code,
        state,
    )
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

fn ensure_expected_version(
    state: &State,
    cell_id: &str,
    expected_version: u64,
) -> Result<(), McpError> {
    if expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook.run_cell expected_version must be >= 1",
            None,
        ));
    }

    let (root, _) = state.get_notebook().snapshot();
    let actual = root
        .cells
        .iter()
        .find_map(|cell| {
            let (id, version) = match cell {
                Cell::Raw(cell) => (
                    &cell.id,
                    cell.metadata.spur.as_ref().map(|spur| spur.version),
                ),
                Cell::Markdown(cell) => (
                    &cell.id,
                    cell.metadata.spur.as_ref().map(|spur| spur.version),
                ),
                Cell::Code(cell) => (
                    &cell.id,
                    cell.metadata.spur.as_ref().map(|spur| spur.version),
                ),
            };
            (id.as_deref() == Some(cell_id)).then_some(version)
        })
        .ok_or_else(|| {
            McpError::invalid_params(
                "notebook.run_cell cell_id was not found",
                Some(json!({ "code": "cell_not_found", "cell_id": cell_id })),
            )
        })?
        .ok_or_else(|| {
            McpError::invalid_params(
                "notebook.run_cell cell has no spur version",
                Some(json!({ "cell_id": cell_id })),
            )
        })?;

    if actual != expected_version {
        return Err(McpError::invalid_params(
            "notebook.run_cell stale_version",
            Some(json!({
                "code": "stale_version",
                "cell_id": cell_id,
                "expected": expected_version,
                "actual": actual,
            })),
        ));
    }

    Ok(())
}

fn ensure_code_cell(state: &State, cell_id: &str) -> Result<(), McpError> {
    let (root, _) = state.get_notebook().snapshot();
    for cell in &root.cells {
        match cell {
            Cell::Raw(cell) if cell.id.as_deref() == Some(cell_id) => {
                return Err(not_code_cell(cell_id));
            }
            Cell::Markdown(cell) if cell.id.as_deref() == Some(cell_id) => {
                return Err(not_code_cell(cell_id));
            }
            Cell::Code(cell) if cell.id.as_deref() == Some(cell_id) => return Ok(()),
            Cell::Raw(_) | Cell::Markdown(_) | Cell::Code(_) => {}
        }
    }

    Err(McpError::invalid_params(
        "notebook.run_cell cell_not_found",
        Some(json!({ "code": "cell_not_found", "cell_id": cell_id })),
    ))
}

fn not_code_cell(cell_id: &str) -> McpError {
    McpError::invalid_params(
        "notebook.run_cell not_code_cell",
        Some(json!({ "code": "not_code_cell", "cell_id": cell_id })),
    )
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
            // Ephemeral compile-progress signal (Phase-1/2 compile ticker):
            // surfaced via the per-event progress report above, not a cell output.
            RunCellEvent::CompileProgress { .. } => {}
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
            Cell, CellMetadata, CodeCell, MarkdownCell, MultilineString, NotebookMetadata,
            NotebookRoot, Output, OutputStream, SpurCellMetadata,
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
                jute_deck: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some("code-1".to_string()),
                metadata: CellMetadata {
                    spur: Some(SpurCellMetadata {
                        version: 1,
                        last_edited_by: None,
                        datasource_setup: None,
                        dag: None,
                        code_type: None,
                    }),
                    jute_deck: None,
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

    fn notebook_with_markdown_cell() -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Markdown(MarkdownCell {
                id: Some("markdown-1".to_string()),
                metadata: CellMetadata {
                    spur: Some(SpurCellMetadata {
                        version: 1,
                        last_edited_by: None,
                        datasource_setup: None,
                        dag: None,
                        code_type: None,
                    }),
                    jute_deck: None,
                    other: Map::new(),
                },
                source: MultilineString::Single("not code".to_string()),
                attachments: None,
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
            json!({
                "cell_id": "code-1",
                "notebook_path": "/tmp/test.ipynb",
                "kernel_id": "missing",
                "code": "1+1"
            }),
        )
        .await
        .expect_err("missing slot reports error");
        assert!(error.message.contains("run_cell"));
    }

    #[tokio::test]
    async fn empty_kernel_id_requires_open_notebook() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_code_cell());
        let deps = deps_with_state(state);
        let error = call(
            &deps,
            json!({
                "cell_id": "code-1",
                "notebook_path": "/tmp/test.ipynb",
                "kernel_id": "",
                "code": ""
            }),
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
    async fn expected_version_rejects_stale_cell_before_dispatch() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_code_cell());
        let deps = deps_with_state(state);

        let error = call(
            &deps,
            json!({
                "cell_id": "code-1",
                "notebook_path": "/tmp/test.ipynb",
                "kernel_id": "missing",
                "code": "print('new')",
                "expected_version": 99
            }),
        )
        .await
        .expect_err("stale expected_version is rejected before kernel dispatch");

        assert!(error.message.contains("stale_version"));
        assert_eq!(
            error.data.and_then(|data| data.get("code").cloned()),
            Some(json!("stale_version"))
        );
    }

    #[tokio::test]
    async fn markdown_cell_rejected_before_dispatch() {
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load("/tmp/test.ipynb", notebook_with_markdown_cell());
        let deps = deps_with_state(state);

        let error = call(
            &deps,
            json!({
                "cell_id": "markdown-1",
                "notebook_path": "/tmp/test.ipynb",
                "kernel_id": "missing",
                "code": "print('new')"
            }),
        )
        .await
        .expect_err("markdown cell is rejected before kernel dispatch");

        assert!(error.message.contains("not_code_cell"));
        assert_eq!(
            error.data.and_then(|data| data.get("code").cloned()),
            Some(json!("not_code_cell"))
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
            notebook_path: "/tmp/test.ipynb".to_string(),
            kernel_id: Some("kernel-1".to_string()),
            code: "print('new')".to_string(),
            expected_version: None,
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
