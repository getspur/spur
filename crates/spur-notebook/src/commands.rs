use std::{collections::BTreeMap, path::Path, sync::Arc};

use jute::backend::notebook::{Cell, CellDagMetadata, NotebookRoot};
use serde_json::{json, Value};

use crate::{
    dag::{
        engine::RunCellCommandRunner, notebook_port_root, NotebookDag, PortStore, ReactiveEngine,
    },
    mcp::{
        bridge::{AgentBridge, TauriBridgeRequester},
        ServerDeps,
    },
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
    let deps = ServerDeps {
        bridge: Arc::new(TauriBridgeRequester::without_app(Arc::clone(
            bridge.inner(),
        ))),
        state: Some(Arc::clone(state.inner())),
        app: None,
        daemon: None,
    };
    let runner = RunCellCommandRunner::new(Arc::new(deps));
    let mut engine = ReactiveEngine::new(notebook, runner, &path, notebook_port_root(&path));
    let report = engine
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
    let deps = ServerDeps {
        bridge: Arc::new(TauriBridgeRequester::without_app(Arc::clone(
            bridge.inner(),
        ))),
        state: Some(Arc::clone(state.inner())),
        app: None,
        daemon: None,
    };
    let runner = RunCellCommandRunner::new(Arc::new(deps));
    let mut engine = ReactiveEngine::new(notebook, runner, &path, notebook_port_root(&path));
    let report = engine.run_cell(&cell_id).await.map_err(|error| {
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
    use std::sync::Arc;

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use jute::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, MultilineString, NotebookMetadata,
        NotebookRoot, PortSpec, SpurCellMetadata,
    };
    use tempfile::TempDir;

    use super::*;

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
}
