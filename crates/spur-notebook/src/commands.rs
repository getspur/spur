use std::{collections::BTreeMap, path::Path, sync::Arc};

use jute::backend::notebook::{Cell, CellDagMetadata, NotebookRoot};
use serde_json::{json, Value};

use crate::{
    dag::{notebook_port_root, notebook_run_context, NotebookDag, PortStore},
    mcp::bridge::{AgentBridge, TauriBridgeRequester},
};

/// Return the active notebook DAG snapshot for the Tauri frontend.
#[tauri::command]
pub async fn notebook_dag_status(
    state: tauri::State<'_, std::sync::Arc<jute::state::State>>,
) -> Result<Value, jute::Error> {
    notebook_dag_status_for_state(&state)
}

/// Run a notebook cell through the reactive DAG engine and cascade downstream cells.
#[tauri::command]
pub async fn notebook_run_cascade(
    state: tauri::State<'_, Arc<jute::state::State>>,
    bridge: tauri::State<'_, Arc<AgentBridge>>,
    cell_id: String,
) -> Result<Value, jute::Error> {
    let notebook = state.get_notebook();
    let path = notebook.path().ok_or_else(|| {
        jute::Error::NotebookDaemon("notebook_run_cascade requires an open notebook".to_string())
    })?;
    let mut context = notebook_run_context(
        &path,
        Arc::clone(state.inner()),
        Arc::new(TauriBridgeRequester::without_app(Arc::clone(
            bridge.inner(),
        ))),
        None,
        None,
    );
    let report = context
        .engine
        .run_cell_and_cascade(&cell_id)
        .await
        .map_err(|error| {
            jute::Error::NotebookDaemon(format!("notebook_run_cascade failed: {error}"))
        })?;

    Ok(json!({
        "cell_id": cell_id,
        "runs": report.runs.into_iter().map(|run| {
            json!({
                "cell_id": run.cell_id,
                "status": run.status.as_str(),
            })
        }).collect::<Vec<_>>(),
    }))
}

/// Run a notebook cell through the reactive DAG engine without cascading downstream cells.
#[tauri::command]
pub async fn notebook_run_cell(
    state: tauri::State<'_, Arc<jute::state::State>>,
    bridge: tauri::State<'_, Arc<AgentBridge>>,
    cell_id: String,
) -> Result<Value, jute::Error> {
    let notebook = state.get_notebook();
    let path = notebook.path().ok_or_else(|| {
        jute::Error::NotebookDaemon("notebook_run_cell requires an open notebook".to_string())
    })?;
    let mut context = notebook_run_context(
        &path,
        Arc::clone(state.inner()),
        Arc::new(TauriBridgeRequester::without_app(Arc::clone(
            bridge.inner(),
        ))),
        None,
        None,
    );
    let report = context.engine.run_cell(&cell_id).await.map_err(|error| {
        jute::Error::NotebookDaemon(format!("notebook_run_cell failed: {error}"))
    })?;

    Ok(json!({
        "cell_id": report.cell_id,
        "status": report.status.as_str(),
    }))
}

fn notebook_dag_status_for_state(state: &jute::state::State) -> Result<Value, jute::Error> {
    let notebook = state.get_notebook();
    let path = notebook.path().ok_or_else(|| {
        jute::Error::NotebookDaemon("notebook_dag_status requires an open notebook".to_string())
    })?;
    let (root, version) = notebook.snapshot();
    notebook_dag_status_snapshot(&root, version, &path)
}

fn notebook_dag_status_snapshot(
    root: &NotebookRoot,
    version: u64,
    path: &Path,
) -> Result<Value, jute::Error> {
    let dag = build_graph(root)?;
    let port_manifest = PortStore::open_read_only_at(notebook_port_root(path))
        .map_err(|error| {
            jute::Error::NotebookDaemon(format!(
                "notebook_dag_status failed to read port manifest: {error}"
            ))
        })?
        .manifest()
        .iter()
        .map(|(port, entry)| (port.clone(), entry.version))
        .collect::<BTreeMap<_, _>>();

    Ok(json!({
        "notebook_version": version,
        "nodes": dag_nodes(root),
        "edges": dag.edges(),
        "port_manifest": port_manifest,
    }))
}

fn build_graph(root: &NotebookRoot) -> Result<NotebookDag, jute::Error> {
    NotebookDag::from_metadata(root.cells.iter().filter_map(|cell| {
        let id = cell_id(cell)?;
        let dag = cell_dag(cell)?;
        Some((id, dag.clone()))
    }))
    .map_err(|error| {
        jute::Error::NotebookDaemon(format!("notebook_dag_status failed to build DAG: {error}"))
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
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use jute::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, MultilineString, NotebookMetadata,
        NotebookRoot, PortSpec, SpurCellMetadata,
    };
    use tempfile::TempDir;

    use crate::{
        dag::{
            engine::{CellRunOutcome, CellRunRequest, CellRunStatus, CellRunner, EngineError},
            notebook_run_context_with_runner,
        },
        mcp::bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    };

    use super::*;

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
            _timeout: Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async { Err(BridgeError::NotebookNotOpen) })
        }
    }

    #[derive(Clone)]
    struct RecordingRunner {
        requests: Arc<Mutex<Vec<CellRunRequest>>>,
    }

    impl CellRunner for RecordingRunner {
        fn run_cell<'a>(
            &'a self,
            request: CellRunRequest,
        ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.requests.lock().expect("requests").push(request);
                Ok(CellRunOutcome {
                    status: CellRunStatus::Succeeded,
                })
            })
        }
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

    fn cell(
        id: &str,
        produces: Vec<&str>,
        consumes: Vec<&str>,
        execution_count: Option<u32>,
    ) -> Cell {
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
                            })
                            .collect(),
                        consumes: consumes.into_iter().map(str::to_string).collect(),
                        source: None,
                    }),
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count,
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

    #[test]
    fn command_snapshot_matches_mcp_payload_shape() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("nb.ipynb");
        let state = jute::state::State::new();
        state.get_notebook().load(
            &notebook_path,
            notebook(vec![
                cell("producer", vec!["raw"], vec![], Some(1)),
                cell("consumer", vec![], vec!["raw"], None),
            ]),
        );
        let port_root = notebook_port_root(&notebook_path);
        PortStore::open_at(&port_root)
            .expect("port store")
            .put("raw", &ipc_batch())
            .expect("put port");

        let snapshot = notebook_dag_status_for_state(&state).expect("status snapshot");

        assert_eq!(snapshot["notebook_version"], 1);
        assert_eq!(snapshot["nodes"][0]["state"]["kind"], "code");
        assert_eq!(snapshot["nodes"][0]["state"]["execution_count"], 1);
        assert_eq!(
            snapshot["nodes"][1]["state"]["execution_count"],
            Value::Null
        );
        assert_eq!(
            snapshot["edges"][0],
            json!({ "producer": "producer", "consumer": "consumer", "port": "raw" })
        );
        assert_eq!(snapshot["port_manifest"]["raw"], 1);
    }

    #[tokio::test]
    async fn command_run_context_resolves_kernel_with_daemon_none_and_dispatches() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("nb.ipynb");
        let state = Arc::new(jute::state::State::new());
        state.get_notebook().load(
            &notebook_path,
            notebook(vec![cell("producer", vec!["raw"], vec![], None)]),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));

        let mut context = notebook_run_context_with_runner(
            &notebook_path,
            Arc::clone(&state),
            Arc::new(TestBridge),
            None,
            None,
            {
                let requests = Arc::clone(&requests);
                move |_deps| RecordingRunner { requests }
            },
        );

        assert!(context.deps.daemon.is_none());
        context.engine.run_cell("producer").await.expect("run cell");

        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].cell_id, "producer");
        assert_eq!(
            requests[0].kernel_id,
            Some(jute::state::notebook_slot_id(
                notebook_path.to_string_lossy().as_ref()
            ))
        );
    }
}
