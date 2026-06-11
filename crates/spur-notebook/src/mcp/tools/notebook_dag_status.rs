use std::collections::BTreeMap;

use jute::backend::notebook::{Cell, CellDagMetadata, NotebookRoot};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use super::parse_no_args;
use crate::{
    dag::{notebook_port_root, NotebookDag, PortStore},
    mcp::ServerDeps,
};

const METHOD: &str = "notebook_dag_status";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return the active notebook DAG, per-cell DAG state, and current port manifest versions.",
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
            "notebook_dag_status requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_dag_status requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_dag_status requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;
    let (root, version) = state.notebook_for_path(&path).snapshot();
    let dag = build_graph(&root)?;
    let port_manifest = PortStore::open_read_only_at(notebook_port_root(&path))
        .map_err(|error| {
            McpError::internal_error(
                "notebook_dag_status failed to read port manifest",
                Some(json!({ "error": error.to_string() })),
            )
        })?
        .manifest()
        .iter()
        .map(|(port, entry)| (port.clone(), entry.version))
        .collect::<BTreeMap<_, _>>();

    Ok(CallToolResult::structured(json!({
        "notebook_version": version,
        "nodes": dag_nodes(&root),
        "edges": dag.edges(),
        "port_manifest": port_manifest,
    })))
}

fn build_graph(root: &NotebookRoot) -> Result<NotebookDag, McpError> {
    NotebookDag::from_metadata(root.cells.iter().filter_map(|cell| {
        let id = cell_id(cell)?;
        let dag = cell_dag(cell)?;
        Some((id, dag.clone()))
    }))
    .map_err(|error| {
        McpError::internal_error(
            "notebook_dag_status failed to build DAG",
            Some(json!({ "error": error.to_string() })),
        )
    })
}

fn dag_nodes(root: &NotebookRoot) -> Vec<Value> {
    root.cells
        .iter()
        .filter_map(|cell| {
            let id = cell_id(cell)?;
            let dag = cell_dag(cell)?;
            Some(json!({
                "id": id,
                "state": {
                    "kind": cell_kind(cell),
                    "version": cell_version(cell),
                    "execution_count": cell_execution_count(cell),
                },
                "dag": dag,
            }))
        })
        .collect()
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_kind(cell: &Cell) -> &'static str {
    match cell {
        Cell::Raw(_) => "raw",
        Cell::Markdown(_) => "markdown",
        Cell::Code(_) => "code",
    }
}

fn cell_dag(cell: &Cell) -> Option<&CellDagMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
    }
}

fn cell_version(cell: &Cell) -> Option<u64> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Code(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
    }
}

fn cell_execution_count(cell: &Cell) -> Option<u32> {
    match cell {
        Cell::Code(cell) => cell.execution_count,
        Cell::Raw(_) | Cell::Markdown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        state::State,
    };
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use crate::{
        dag::PortStore,
        mcp::{
            bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
            DaemonWindowOps, NotebookDaemonControl, ServerDeps,
        },
    };

    #[derive(Default)]
    struct TestBridge;

    impl BridgeRequester for TestBridge {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            _method: &'static str,
            _params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async { Ok(Value::Null) })
        }
    }

    #[derive(Default)]
    struct TestWindows;

    impl DaemonWindowOps for TestWindows {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, _label: &str) {}

        fn open_notebook_path(&self, _path: &Path) -> Result<String, BridgeError> {
            Ok("test".to_string())
        }

        fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

        fn exit(&self) {}
    }

    fn notebook(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn cell(id: &str, produces: Vec<&str>, consumes: Vec<&str>, source: Option<DagSource>) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 7,
                    last_edited_by: Some("brain".to_string()),
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: produces
                            .into_iter()
                            .map(|port| PortSpec {
                                port: port.to_string(),
                                repr: "arrow".to_string(),
                                display: None,
                                class: None,
                                schema: None,
                            })
                            .collect(),
                        consumes: consumes.into_iter().map(str::to_string).collect(),
                        source,
                    }),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: Some(3),
            outputs: Vec::new(),
        })
    }

    fn ipc_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "value",
            DataType::Int64,
            false,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1]))]).expect("batch")
    }

    async fn deps(root: NotebookRoot, temp: &TempDir) -> ServerDeps {
        let notebook_path = temp.path().join("nb.ipynb");
        let state = Arc::new(State::new());
        state.get_notebook().load(&notebook_path, root);
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            Arc::new(TestBridge),
            Arc::clone(&state),
            Arc::new(TestWindows),
            None,
        );
        control.set_current_path_for_test(notebook_path).await;
        ServerDeps {
            bridge: Arc::new(TestBridge),
            state: Some(state),
            app: None,
            daemon: Some(control),
            plugins: None,
        }
    }

    #[tokio::test]
    async fn returns_dag_edges_cell_state_and_port_versions_without_writing() {
        let temp = TempDir::new().expect("temp dir");
        let root = notebook(vec![
            cell(
                "source",
                vec!["raw"],
                vec![],
                Some(DagSource {
                    kind: "csv".to_string(),
                    port: "sales".to_string(),
                    class: None,
                    schema: None,
                }),
            ),
            cell("consumer", vec![], vec!["raw"], None),
        ]);
        let deps = deps(root, &temp).await;
        let port_root = crate::dag::notebook_port_root(temp.path().join("nb.ipynb"));
        PortStore::open_at(&port_root)
            .expect("port store")
            .put("sales", &ipc_batch())
            .expect("put port");
        let before = std::fs::read_dir(port_root.join("ports"))
            .expect("ports dir exists")
            .count();

        let result = super::call(&deps, json!({}))
            .await
            .expect("status succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(
            body["edges"][0],
            json!({ "producer": "source", "consumer": "consumer", "port": "raw" })
        );
        assert_eq!(body["nodes"][0]["id"], "source");
        assert_eq!(body["nodes"][1]["id"], "consumer");
        assert_eq!(body["nodes"][1]["state"]["version"], 7);
        assert_eq!(body["port_manifest"]["sales"], 1);
        let after = std::fs::read_dir(port_root.join("ports"))
            .expect("ports dir still exists")
            .count();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn snapshots_current_path_store_not_focused_store() {
        let temp = TempDir::new().expect("temp dir");
        let current_path = temp.path().join("current.ipynb");
        let focused_path = temp.path().join("focused.ipynb");
        let state = Arc::new(State::new());
        state.notebook_for_path(&current_path).load(
            &current_path,
            notebook(vec![
                cell("source", vec!["raw"], vec![], None),
                cell("consumer", vec![], vec!["raw"], None),
            ]),
        );
        state.get_notebook().load(
            &focused_path,
            notebook(vec![cell("focused", vec![], vec![], None)]),
        );
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            Arc::new(TestBridge),
            Arc::clone(&state),
            Arc::new(TestWindows),
            None,
        );
        control.set_current_path_for_test(current_path).await;
        let deps = ServerDeps {
            bridge: Arc::new(TestBridge),
            state: Some(state),
            app: None,
            daemon: Some(control),
            plugins: None,
        };

        let result = super::call(&deps, json!({}))
            .await
            .expect("status snapshots current path store");
        let body = result.structured_content.expect("structured content");

        assert_eq!(body["nodes"][0]["id"], "source");
        assert_eq!(body["nodes"][1]["id"], "consumer");
    }
}
