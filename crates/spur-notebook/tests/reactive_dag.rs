use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use jute::{
    backend::{
        commands::RunCellEvent,
        notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, Output, PortSpec, SpurCellMetadata,
        },
    },
    notebook_store::{NotebookOp, NotebookStore},
    state::State,
};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use spur_notebook::{
    dag::{
        engine::{CellRunOutcome, CellRunner, EngineError, KernelEnsureRequest},
        CellRunReport, CellRunRequest, CellRunStatus, PortStore, ReactiveEngine,
        ReactiveEngineClient, SourcePush,
    },
    mcp::{
        bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
        tools::notebook_push_source,
        DaemonWindowOps, NotebookDaemonControl, ServerDeps,
    },
};
use tempfile::TempDir;
use tokio::sync::mpsc;

#[derive(Clone)]
struct StoreBackedRunner {
    store: Arc<NotebookStore>,
    requests: Arc<StdMutex<Vec<CellRunRequest>>>,
    outcomes: Arc<StdMutex<BTreeMap<String, VecDeque<RunnerAction>>>>,
}

#[derive(Clone)]
enum RunnerAction {
    Succeed,
    Fail,
    StaleAfterEdit { source: String },
}

impl StoreBackedRunner {
    fn new(store: Arc<NotebookStore>) -> Self {
        Self {
            store,
            requests: Arc::new(StdMutex::new(Vec::new())),
            outcomes: Arc::new(StdMutex::new(BTreeMap::new())),
        }
    }

    fn push_action(&self, cell_id: &str, action: RunnerAction) {
        self.outcomes
            .lock()
            .expect("outcomes lock")
            .entry(cell_id.to_string())
            .or_default()
            .push_back(action);
    }

    fn requests(&self) -> Vec<CellRunRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl CellRunner for StoreBackedRunner {
    fn run_cell<'a>(
        &'a self,
        request: CellRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            let action = self
                .outcomes
                .lock()
                .expect("outcomes lock")
                .get_mut(&request.cell_id)
                .and_then(VecDeque::pop_front)
                .unwrap_or(RunnerAction::Succeed);

            match action {
                RunnerAction::Succeed => {
                    self.store
                        .apply_run_event(&request.cell_id, RunCellEvent::Started)
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    self.store
                        .apply_run_event(
                            &request.cell_id,
                            RunCellEvent::Stdout(format!("{} => ok\n", request.code)),
                        )
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    self.store
                        .apply_run_event(
                            &request.cell_id,
                            RunCellEvent::Finished {
                                exec_count: Some(request.expected_version as u32),
                                status: "ok".to_string(),
                            },
                        )
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    Ok(CellRunOutcome {
                        status: CellRunStatus::Succeeded,
                    })
                }
                RunnerAction::Fail => {
                    self.store
                        .apply_run_event(&request.cell_id, RunCellEvent::Started)
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    self.store
                        .apply_run_event(
                            &request.cell_id,
                            RunCellEvent::Finished {
                                exec_count: Some(request.expected_version as u32),
                                status: "error".to_string(),
                            },
                        )
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    Ok(CellRunOutcome {
                        status: CellRunStatus::Failed,
                    })
                }
                RunnerAction::StaleAfterEdit { source } => {
                    self.store
                        .apply(NotebookOp::WriteCell {
                            id: request.cell_id.clone(),
                            source,
                            expected_version: Some(request.expected_version),
                            last_edited_by: Some("human".to_string()),
                        })
                        .map_err(|error| EngineError::RunCell(error.to_string()))?;
                    Err(EngineError::StaleCell {
                        cell_id: request.cell_id,
                    })
                }
            }
        })
    }

    fn ensure_kernel<'a>(
        &'a self,
        _request: KernelEnsureRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
struct TestBridge;

impl BridgeRequester for TestBridge {
    fn listener_registered(&self) -> bool {
        false
    }

    fn window_alive(&self) -> bool {
        false
    }

    fn notebook_open(&self) -> bool {
        false
    }

    fn request<'a>(
        &'a self,
        _method: &'static str,
        _params: Value,
        _timeout: Duration,
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
        Ok("test-window".to_string())
    }

    fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

    fn exit(&self) {}
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
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
            other: Map::new(),
        },
        nbformat_minor: 5,
        nbformat: 4,
        cells,
    }
}

fn code_cell(id: &str, source: &str, version: u64, dag: CellDagMetadata) -> Cell {
    Cell::Code(CodeCell {
        id: Some(id.to_string()),
        metadata: CellMetadata {
            spur: Some(SpurCellMetadata {
                version,
                last_edited_by: None,
                datasource_setup: None,
                dag: Some(dag),
                code_type: None,
                frontend: None,
            }),
            jute_deck: None,
            other: Map::new(),
        },
        source: MultilineString::Single(source.to_string()),
        execution_count: None,
        outputs: Vec::new(),
    })
}

fn dag(produces: &[&str], consumes: &[&str], source: Option<DagSource>) -> CellDagMetadata {
    CellDagMetadata {
        produces: produces.iter().map(|port| port_spec(port)).collect(),
        consumes: consumes.iter().map(|port| (*port).to_string()).collect(),
        source,
    }
}

fn port_spec(port: &str) -> PortSpec {
    PortSpec {
        port: port.to_string(),
        repr: "arrow".to_string(),
        display: None,
    }
}

fn source(kind: &str, port: &str) -> DagSource {
    DagSource {
        kind: kind.to_string(),
        port: port.to_string(),
    }
}

fn ipc_bytes() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int64,
        false,
    )]));
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1]))])
        .expect("batch");
    let mut bytes = Vec::new();
    {
        let mut writer =
            arrow_ipc::writer::FileWriter::try_new(&mut bytes, schema.as_ref()).expect("writer");
        writer.write(&batch).expect("write batch");
        writer.finish().expect("finish");
    }
    bytes
}

async fn harness(root: NotebookRoot) -> (ServerDeps, mpsc::Receiver<SourcePush>, TempDir, PathBuf) {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("reactive.ipynb");
    tokio::fs::write(
        &path,
        serde_json::to_vec_pretty(&root).expect("notebook serializes"),
    )
    .await
    .expect("notebook writes");

    let state = Arc::new(State::new());
    state.get_notebook().load(path.clone(), root);
    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        Arc::new(TestBridge),
        Arc::clone(&state),
        Arc::new(TestWindows),
        None,
    );
    control.set_current_path_for_test(path.clone()).await;
    let (tx, rx) = mpsc::channel(4);
    control
        .set_reactive_engine_client(ReactiveEngineClient::new_for_test(tx))
        .await;

    (
        ServerDeps {
            bridge: Arc::new(TestBridge),
            state: Some(state),
            app: None,
            daemon: Some(control),
        },
        rx,
        temp,
        path,
    )
}

fn cell_stdout(root: &NotebookRoot, cell_id: &str) -> String {
    let cell = root
        .cells
        .iter()
        .find(|cell| match cell {
            Cell::Raw(cell) => cell.id.as_deref() == Some(cell_id),
            Cell::Markdown(cell) => cell.id.as_deref() == Some(cell_id),
            Cell::Code(cell) => cell.id.as_deref() == Some(cell_id),
        })
        .expect("cell exists");
    let Cell::Code(cell) = cell else {
        panic!("expected code cell");
    };
    cell.outputs
        .iter()
        .filter_map(|output| match output {
            Output::Stream(output) if output.name == "stdout" => {
                Some(String::from(output.text.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn push_source_reruns_only_downstream_cells_in_dependency_order() {
    let (deps, mut rx, temp, path) = harness(notebook(vec![
        code_cell(
            "source",
            "source()",
            1,
            dag(&["raw"], &[], Some(source("csv", "sales"))),
        ),
        code_cell("left", "left()", 1, dag(&["left"], &["raw"], None)),
        code_cell("join", "join()", 1, dag(&[], &["left"], None)),
        code_cell(
            "independent",
            "independent()",
            1,
            dag(&["other"], &[], Some(source("csv", "inventory"))),
        ),
    ]))
    .await;
    let store = deps.state.as_ref().expect("state").get_notebook();
    let runner = StoreBackedRunner::new(Arc::clone(&store));

    let body = structured(
        notebook_push_source::call(&deps, json!({ "port": "sales", "payload": ipc_bytes() }))
            .await
            .expect("push source accepted"),
    );
    assert_eq!(body["accepted"], true);
    let push = rx.recv().await.expect("source push queued");
    let mut engine = ReactiveEngine::new(store, runner.clone(), path, temp.path().join("ports"));

    engine
        .process_source_push(push)
        .await
        .expect("reactive cascade succeeds");

    let requests = runner
        .requests()
        .into_iter()
        .map(|request| request.cell_id)
        .collect::<Vec<_>>();
    assert_eq!(requests, ["source", "left", "join"]);
}

#[tokio::test]
async fn run_cell_cascade_reruns_target_then_downstream_cells() {
    let (deps, _rx, temp, path) = harness(notebook(vec![
        code_cell("source", "source()", 1, dag(&["raw"], &[], None)),
        code_cell("left", "left()", 1, dag(&["left"], &["raw"], None)),
        code_cell("join", "join()", 1, dag(&[], &["left"], None)),
        code_cell(
            "independent",
            "independent()",
            1,
            dag(&["other"], &[], None),
        ),
    ]))
    .await;
    let store = deps.state.as_ref().expect("state").get_notebook();
    let runner = StoreBackedRunner::new(Arc::clone(&store));
    let port_root = temp.path().join("ports");
    let mut ports = PortStore::open_at(&port_root).expect("open ports");
    ports.put("raw", &ipc_bytes()).expect("seed raw");
    ports.put("left", &ipc_bytes()).expect("seed left");
    let mut engine = ReactiveEngine::new(store, runner.clone(), path, port_root.clone());

    let report = engine
        .run_cell_and_cascade("source")
        .await
        .expect("run cascade succeeds");

    assert_eq!(
        report.runs,
        vec![
            CellRunReport::new("source", CellRunStatus::Succeeded),
            CellRunReport::new("left", CellRunStatus::Succeeded),
            CellRunReport::new("join", CellRunStatus::Succeeded),
        ]
    );
    let requests = runner
        .requests()
        .into_iter()
        .map(|request| request.cell_id)
        .collect::<Vec<_>>();
    assert_eq!(requests, ["source", "left", "join"]);
    let ports = PortStore::open_at(&port_root).expect("open ports");
    assert_eq!(ports.get("raw").expect("raw bumped").version, 2);
    assert_eq!(ports.get("left").expect("left unchanged").version, 1);
}

#[tokio::test]
async fn run_cell_cascade_marks_descendants_upstream_failed() {
    let (deps, _rx, temp, path) = harness(notebook(vec![
        code_cell("source", "source()", 1, dag(&["raw"], &[], None)),
        code_cell("left", "left()", 1, dag(&["left"], &["raw"], None)),
        code_cell("join", "join()", 1, dag(&[], &["left"], None)),
    ]))
    .await;
    let store = deps.state.as_ref().expect("state").get_notebook();
    let runner = StoreBackedRunner::new(Arc::clone(&store));
    runner.push_action("left", RunnerAction::Fail);
    let port_root = temp.path().join("ports");
    let mut ports = PortStore::open_at(&port_root).expect("open ports");
    ports.put("raw", &ipc_bytes()).expect("seed raw");
    ports.put("left", &ipc_bytes()).expect("seed left");
    let mut engine = ReactiveEngine::new(store, runner.clone(), path, port_root);

    let report = engine
        .run_cell_and_cascade("source")
        .await
        .expect("run cascade succeeds");

    assert_eq!(
        report.runs,
        vec![
            CellRunReport::new("source", CellRunStatus::Succeeded),
            CellRunReport::new("left", CellRunStatus::Failed),
            CellRunReport::new("join", CellRunStatus::UpstreamFailed),
        ]
    );
    let requests = runner
        .requests()
        .into_iter()
        .map(|request| request.cell_id)
        .collect::<Vec<_>>();
    assert_eq!(requests, ["source", "left"]);
}

#[test]
fn dag_notebook_remains_valid_for_vanilla_nbformat_readers() {
    #[derive(Debug, Deserialize)]
    struct VanillaNotebook {
        nbformat: u8,
        nbformat_minor: u8,
        cells: Vec<VanillaCell>,
    }

    #[derive(Debug, Deserialize)]
    struct VanillaCell {
        cell_type: String,
        metadata: VanillaMetadata,
        source: Value,
        execution_count: Option<u32>,
        outputs: Option<Vec<VanillaOutput>>,
    }

    #[derive(Debug, Deserialize)]
    struct VanillaMetadata {}

    #[derive(Debug, Deserialize)]
    #[serde(tag = "output_type", rename_all = "snake_case")]
    enum VanillaOutput {
        Stream { name: String, text: MultilineString },
    }

    let root = notebook(vec![code_cell(
        "source",
        "print(42)",
        1,
        dag(&["raw"], &[], Some(source("csv", "sales"))),
    )]);
    let mut root_value = serde_json::to_value(root).expect("notebook serializes");
    root_value["cells"][0]["execution_count"] = json!(3);
    root_value["cells"][0]["outputs"] = json!([
        {
            "output_type": "stream",
            "name": "stdout",
            "text": "42\n"
        }
    ]);
    let parsed: VanillaNotebook =
        serde_json::from_value(root_value).expect("vanilla notebook reader parses DAG notebook");

    assert_eq!(parsed.nbformat, 4);
    assert_eq!(parsed.nbformat_minor, 5);
    assert_eq!(parsed.cells.len(), 1);
    assert_eq!(parsed.cells[0].cell_type, "code");
    assert_eq!(parsed.cells[0].execution_count, Some(3));
    assert_eq!(parsed.cells[0].source, json!("print(42)"));
    let outputs = parsed.cells[0].outputs.as_ref().expect("outputs");
    let VanillaOutput::Stream { name, text } = &outputs[0];
    assert_eq!(name, "stdout");
    assert_eq!(String::from(text.clone()), "42\n");
    let _ignored = &parsed.cells[0].metadata;
}

#[tokio::test]
async fn headless_source_push_autosaves_new_outputs_to_disk() {
    let (deps, mut rx, temp, path) = harness(notebook(vec![code_cell(
        "source",
        "source()",
        1,
        dag(&["raw"], &[], Some(source("csv", "sales"))),
    )]))
    .await;
    let store = deps.state.as_ref().expect("state").get_notebook();
    let runner = StoreBackedRunner::new(Arc::clone(&store));

    notebook_push_source::call(&deps, json!({ "port": "sales", "payload": ipc_bytes() }))
        .await
        .expect("push source accepted");
    let push = rx.recv().await.expect("source push queued");
    let mut engine = ReactiveEngine::new(store, runner, path.clone(), temp.path().join("ports"));

    engine
        .process_source_push(push)
        .await
        .expect("reactive cascade succeeds");

    tokio::time::sleep(Duration::from_millis(900)).await;
    let saved: NotebookRoot =
        serde_json::from_slice(&tokio::fs::read(&path).await.expect("notebook file reads"))
            .expect("saved notebook parses");

    assert_eq!(cell_stdout(&saved, "source"), "source() => ok\n");
}

#[tokio::test]
async fn mid_run_human_edit_causes_stale_retry_without_clobbering_cell() {
    let (deps, mut rx, temp, path) = harness(notebook(vec![code_cell(
        "source",
        "old_source()",
        1,
        dag(&["raw"], &[], Some(source("csv", "sales"))),
    )]))
    .await;
    let store = deps.state.as_ref().expect("state").get_notebook();
    let runner = StoreBackedRunner::new(Arc::clone(&store));
    runner.push_action(
        "source",
        RunnerAction::StaleAfterEdit {
            source: "human_source()".to_string(),
        },
    );

    notebook_push_source::call(&deps, json!({ "port": "sales", "payload": ipc_bytes() }))
        .await
        .expect("push source accepted");
    let push = rx.recv().await.expect("source push queued");
    let mut engine = ReactiveEngine::new(
        store.clone(),
        runner.clone(),
        path,
        temp.path().join("ports"),
    );

    engine
        .process_source_push(push)
        .await
        .expect("reactive cascade succeeds");

    let requests = runner.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].expected_version, 1);
    assert_eq!(requests[0].code, "old_source()");
    assert_eq!(requests[1].expected_version, 2);
    assert_eq!(requests[1].code, "human_source()");

    let (snapshot, _version) = store.snapshot();
    let Cell::Code(cell) = &snapshot.cells[0] else {
        panic!("expected code cell");
    };
    assert_eq!(String::from(cell.source.clone()), "human_source()");
    assert_eq!(cell_stdout(&snapshot, "source"), "human_source() => ok\n");
}
