use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use jute::{
    backend::notebook::{Cell, CellDagMetadata, DagSource, NotebookRoot},
    notebook_store::{DeltaKind, NotebookDelta, NotebookStore},
};
use serde_json::json;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::{debug, warn};

use crate::mcp::{tools::run_cell, ServerDeps};

use super::{notebook_port_root, NotebookDag, PortStore, PortStoreError};

const DEFAULT_SOURCE_DEBOUNCE: Duration = Duration::from_millis(150);
const DEFAULT_MAX_IN_FLIGHT: usize = 4;
const STALE_RETRY_LIMIT: usize = 3;

#[derive(Debug, Clone)]
pub struct ReactiveEngineConfig {
    pub source_debounce: Duration,
    pub max_in_flight: usize,
}

impl Default for ReactiveEngineConfig {
    fn default() -> Self {
        Self {
            source_debounce: DEFAULT_SOURCE_DEBOUNCE,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePush {
    pub source: DagSource,
    pub ipc_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRunRequest {
    pub cell_id: String,
    pub kernel_id: Option<String>,
    pub code: String,
    pub expected_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRunOutcome {
    pub status: CellRunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellRunStatus {
    Succeeded,
    Failed,
    UpstreamFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRunReport {
    pub cell_id: String,
    pub status: CellRunStatus,
}

impl CellRunReport {
    pub fn new(cell_id: impl Into<String>, status: CellRunStatus) -> Self {
        Self {
            cell_id: cell_id.into(),
            status,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CascadeReport {
    pub runs: Vec<CellRunReport>,
}

pub trait CellRunner: Clone + Send + Sync + 'static {
    fn run_cell<'a>(
        &'a self,
        request: CellRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct RunCellCommandRunner {
    deps: Arc<ServerDeps>,
}

impl RunCellCommandRunner {
    pub fn new(deps: Arc<ServerDeps>) -> Self {
        Self { deps }
    }
}

impl CellRunner for RunCellCommandRunner {
    fn run_cell<'a>(
        &'a self,
        request: CellRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>> {
        Box::pin(async move {
            let cell_id = request.cell_id.clone();
            let result = run_cell::call(
                &self.deps,
                json!({
                    "cell_id": request.cell_id,
                    "kernel_id": request.kernel_id,
                    "code": request.code,
                    "expected_version": request.expected_version,
                }),
            )
            .await
            .map_err(|error| {
                if error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("code"))
                    .and_then(|code| code.as_str())
                    == Some("stale_version")
                {
                    EngineError::StaleCell { cell_id }
                } else {
                    EngineError::RunCell(format!("{error:?}"))
                }
            })?;
            let body = result.structured_content.ok_or_else(|| {
                EngineError::RunCell("run_cell returned no structured content".to_owned())
            })?;
            let status = body
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("error");
            Ok(CellRunOutcome {
                status: if status == "finished" || status == "ok" || status == "idle" {
                    CellRunStatus::Succeeded
                } else {
                    CellRunStatus::Failed
                },
            })
        })
    }
}

#[derive(Debug)]
pub enum EngineError {
    Dag(crate::dag::DagError),
    Port(PortStoreError),
    CellNotFound { cell_id: String },
    MissingCellVersion { cell_id: String },
    StaleCell { cell_id: String },
    RunCell(String),
    DaemonUnavailable,
    SourceQueueClosed,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dag(error) => write!(f, "{error}"),
            Self::Port(error) => write!(f, "{error}"),
            Self::CellNotFound { cell_id } => write!(f, "cell not found: {cell_id}"),
            Self::MissingCellVersion { cell_id } => {
                write!(f, "cell has no spur version: {cell_id}")
            }
            Self::StaleCell { cell_id } => write!(f, "stale cell version: {cell_id}"),
            Self::RunCell(error) => write!(f, "run_cell failed: {error}"),
            Self::DaemonUnavailable => write!(f, "notebook daemon state is unavailable"),
            Self::SourceQueueClosed => write!(f, "reactive engine source queue is closed"),
        }
    }
}

impl Error for EngineError {}

impl From<crate::dag::DagError> for EngineError {
    fn from(error: crate::dag::DagError) -> Self {
        Self::Dag(error)
    }
}

impl From<PortStoreError> for EngineError {
    fn from(error: PortStoreError) -> Self {
        Self::Port(error)
    }
}

pub struct ReactiveEngine<R = RunCellCommandRunner>
where
    R: CellRunner,
{
    store: Arc<NotebookStore>,
    runner: R,
    port_root: PathBuf,
    graph: Option<NotebookDag>,
}

impl<R> ReactiveEngine<R>
where
    R: CellRunner,
{
    pub fn new(store: Arc<NotebookStore>, runner: R, port_root: PathBuf) -> Self {
        Self {
            store,
            runner,
            port_root,
            graph: None,
        }
    }

    pub fn rebuild_graph(&mut self) -> Result<(), EngineError> {
        let (root, _) = self.store.snapshot();
        self.graph = Some(build_graph(&root)?);
        Ok(())
    }

    pub async fn process_source_push(
        &mut self,
        push: SourcePush,
    ) -> Result<CascadeReport, EngineError> {
        self.rebuild_graph()?;

        let mut ports = PortStore::open_at(&self.port_root)?;
        ports.put(&push.source.port, &push.ipc_bytes)?;

        let graph = self.graph.as_ref().expect("graph was rebuilt");
        let stale = graph.stale_from_source(&push.source)?;
        let mut blocked = BTreeSet::new();
        let mut report = CascadeReport::default();

        for cell_id in stale {
            if blocked.contains(&cell_id) {
                report.runs.push(CellRunReport::new(
                    cell_id.clone(),
                    CellRunStatus::UpstreamFailed,
                ));
                blocked.extend(self.downstream_of(&cell_id)?);
                continue;
            }

            let outcome = self.run_cell_with_retries(&cell_id).await?;
            let status = outcome.status;
            report
                .runs
                .push(CellRunReport::new(cell_id.clone(), status));
            if status == CellRunStatus::Failed {
                blocked.extend(self.downstream_of(&cell_id)?);
            }
        }

        Ok(report)
    }

    async fn run_cell_with_retries(
        &mut self,
        cell_id: &str,
    ) -> Result<CellRunOutcome, EngineError> {
        let mut attempts = 0;
        loop {
            let request = self.cell_run_request(cell_id)?;
            match self.runner.run_cell(request).await {
                Ok(outcome) => return Ok(outcome),
                Err(EngineError::StaleCell { .. }) if attempts < STALE_RETRY_LIMIT => {
                    attempts += 1;
                    self.rebuild_graph()?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn cell_run_request(&self, cell_id: &str) -> Result<CellRunRequest, EngineError> {
        let (root, _) = self.store.snapshot();
        let cell = cell_view(&root, cell_id).ok_or_else(|| EngineError::CellNotFound {
            cell_id: cell_id.to_owned(),
        })?;
        let expected_version = cell
            .version
            .ok_or_else(|| EngineError::MissingCellVersion {
                cell_id: cell_id.to_owned(),
            })?;
        Ok(CellRunRequest {
            cell_id: cell_id.to_owned(),
            kernel_id: None,
            code: cell.source,
            expected_version,
        })
    }

    fn downstream_of(&self, cell_id: &str) -> Result<BTreeSet<String>, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(BTreeSet::new());
        };
        let Some(metadata) = graph.cell_metadata(cell_id) else {
            return Ok(BTreeSet::new());
        };
        let mut downstream = BTreeSet::new();
        for produced in &metadata.produces {
            downstream.extend(graph.stale_from_port(&produced.port)?);
        }
        Ok(downstream)
    }
}

pub struct ReactiveEngineHandle {
    source_tx: mpsc::Sender<SourcePush>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct ReactiveEngineClient {
    source_tx: mpsc::Sender<SourcePush>,
}

impl ReactiveEngineClient {
    pub(crate) fn new(source_tx: mpsc::Sender<SourcePush>) -> Self {
        Self { source_tx }
    }

    pub async fn push_source(&self, push: SourcePush) -> Result<(), EngineError> {
        self.source_tx
            .send(push)
            .await
            .map_err(|_send_error| EngineError::SourceQueueClosed)
    }
}

impl ReactiveEngineHandle {
    pub fn client(&self) -> ReactiveEngineClient {
        ReactiveEngineClient::new(self.source_tx.clone())
    }

    pub async fn push_source(&self, push: SourcePush) -> Result<(), EngineError> {
        self.source_tx
            .send(push)
            .await
            .map_err(|_send_error| EngineError::SourceQueueClosed)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for ReactiveEngineHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.task.abort();
    }
}

pub fn spawn_reactive_engine(
    deps: Arc<ServerDeps>,
    config: ReactiveEngineConfig,
) -> Result<ReactiveEngineHandle, EngineError> {
    let state = Arc::clone(deps.state.as_ref().ok_or(EngineError::DaemonUnavailable)?);
    let store = state.get_notebook();
    let mut deltas = store.subscribe();
    let (source_tx, mut source_rx) = mpsc::channel(128);
    let (complete_tx, mut complete_rx) = mpsc::channel(128);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let runner = RunCellCommandRunner::new(Arc::clone(&deps));

    let task = tokio::spawn(async move {
        let mut debounce = SourceDebounce::new(config.clone());
        let mut in_flight = 0usize;
        let mut resident_graph: Option<NotebookDag> = None;

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                completed = complete_rx.recv() => {
                    if completed.is_some() {
                        in_flight = in_flight.saturating_sub(1);
                    }
                }
                delta = deltas.recv() => {
                    match delta {
                        Ok(delta) if is_structural_delta(&delta) => {
                            let (root, _) = store.snapshot();
                            match build_graph(&root) {
                                Ok(graph) => resident_graph = Some(graph),
                                Err(error) => warn!(%error, "reactive engine graph rebuild failed"),
                            }
                            debug!(
                                version = delta.version,
                                edges = resident_graph.as_ref().map(|graph| graph.edges().len()).unwrap_or(0),
                                "reactive engine observed structural notebook delta"
                            );
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            debug!(skipped, "reactive engine delta subscriber lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                maybe_push = source_rx.recv() => {
                    let Some(push) = maybe_push else { break };
                    debounce.push(push);
                }
                () = tokio::time::sleep(config.source_debounce), if debounce.pending_len() > 0 => {
                    let available = debounce.available_slots(in_flight);
                    for push in debounce.drain_ready(available) {
                        let Some(daemon) = deps.daemon.as_ref() else {
                            warn!("reactive source push dropped because daemon control is unavailable");
                            continue;
                        };
                        let Some(path) = daemon.current_path().await else {
                            warn!("reactive source push dropped because no notebook is open");
                            continue;
                        };
                        in_flight += 1;
                        let store = Arc::clone(&store);
                        let runner = runner.clone();
                        let port_root = notebook_port_root(path);
                        let complete_tx = complete_tx.clone();
                        tokio::spawn(async move {
                            let mut engine = ReactiveEngine::new(store, runner, port_root);
                            if let Err(error) = engine.process_source_push(push).await {
                                warn!(%error, "reactive source cascade failed");
                            }
                            let _ = complete_tx.send(()).await;
                        });
                    }
                }
            }
        }
    });

    Ok(ReactiveEngineHandle {
        source_tx,
        shutdown_tx: Some(shutdown_tx),
        task,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    kind: String,
    port: String,
}

impl From<&DagSource> for SourceKey {
    fn from(source: &DagSource) -> Self {
        Self {
            kind: source.kind.clone(),
            port: source.port.clone(),
        }
    }
}

struct SourceDebounce {
    config: ReactiveEngineConfig,
    pending: BTreeMap<SourceKey, SourcePush>,
}

impl SourceDebounce {
    fn new(config: ReactiveEngineConfig) -> Self {
        let config = ReactiveEngineConfig {
            source_debounce: config.source_debounce,
            max_in_flight: config.max_in_flight.max(1),
        };
        Self {
            config,
            pending: BTreeMap::new(),
        }
    }

    fn push(&mut self, push: SourcePush) {
        self.pending.insert(SourceKey::from(&push.source), push);
    }

    fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn available_slots(&self, in_flight: usize) -> usize {
        self.config.max_in_flight.saturating_sub(in_flight)
    }

    fn drain_ready(&mut self, available: usize) -> Vec<SourcePush> {
        let keys = self
            .pending
            .keys()
            .take(available)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| self.pending.remove(&key))
            .collect()
    }
}

fn is_structural_delta(delta: &NotebookDelta) -> bool {
    matches!(
        delta.kind,
        DeltaKind::Loaded { .. }
            | DeltaKind::CellWritten { .. }
            | DeltaKind::CellInserted { .. }
            | DeltaKind::CellDeleted { .. }
    )
}

fn build_graph(root: &NotebookRoot) -> Result<NotebookDag, EngineError> {
    NotebookDag::from_metadata(root.cells.iter().filter_map(|cell| {
        let id = cell_id(cell)?;
        let dag = cell_dag(cell)?;
        Some((id, dag.clone()))
    }))
    .map_err(EngineError::Dag)
}

struct CellView {
    source: String,
    version: Option<u64>,
}

fn cell_view(root: &NotebookRoot, target: &str) -> Option<CellView> {
    root.cells.iter().find_map(|cell| {
        (cell_id(cell).as_deref() == Some(target)).then(|| CellView {
            source: cell_source(cell),
            version: cell_version(cell),
        })
    })
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
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

fn cell_source(cell: &Cell) -> String {
    match cell {
        Cell::Raw(cell) => String::from(cell.source.clone()),
        Cell::Markdown(cell) => String::from(cell.source.clone()),
        Cell::Code(cell) => String::from(cell.source.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        collections::{BTreeMap, VecDeque},
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        commands::SaveCoordinator,
        notebook_store::NotebookStore,
    };
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct FakeRunner {
        requests: Arc<Mutex<Vec<CellRunRequest>>>,
        outcomes: Arc<Mutex<BTreeMap<String, VecDeque<Result<CellRunOutcome, EngineError>>>>>,
    }

    impl FakeRunner {
        fn fail_once(&self, cell_id: &str) {
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .entry(cell_id.to_string())
                .or_default()
                .push_back(Err(EngineError::StaleCell {
                    cell_id: cell_id.to_string(),
                }));
        }

        fn fail_run(&self, cell_id: &str) {
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .entry(cell_id.to_string())
                .or_default()
                .push_back(Ok(CellRunOutcome {
                    status: CellRunStatus::Failed,
                }));
        }

        fn requests(&self) -> Vec<CellRunRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl CellRunner for FakeRunner {
        fn run_cell<'a>(
            &'a self,
            request: CellRunRequest,
        ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.requests
                    .lock()
                    .expect("requests lock")
                    .push(request.clone());
                let outcome = self
                    .outcomes
                    .lock()
                    .expect("outcomes lock")
                    .get_mut(&request.cell_id)
                    .and_then(VecDeque::pop_front);
                outcome.unwrap_or(Ok(CellRunOutcome {
                    status: CellRunStatus::Succeeded,
                }))
            })
        }
    }

    #[tokio::test]
    async fn source_push_runs_stale_cells_in_order_and_isolates_failed_branch() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![
            cell(
                "a",
                "a = spur.get('sales')",
                1,
                dag(vec![port("a")], vec![], Some(source("csv", "sales"))),
            ),
            cell("b", "b = a", 1, dag(vec![port("b")], vec!["a"], None)),
            cell("c", "c = a", 1, dag(vec![port("c")], vec!["a"], None)),
            cell("d", "d = b", 1, dag(vec![], vec!["b"], None)),
        ]));
        let runner = FakeRunner::default();
        runner.fail_run("b");
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().to_path_buf(),
        );

        let report = engine
            .process_source_push(SourcePush {
                source: source("csv", "sales"),
                ipc_bytes: ipc_bytes(),
            })
            .await
            .expect("source push");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("a", CellRunStatus::Succeeded),
                CellRunReport::new("b", CellRunStatus::Failed),
                CellRunReport::new("c", CellRunStatus::Succeeded),
                CellRunReport::new("d", CellRunStatus::UpstreamFailed),
            ]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            PortStore::open_at(temp.path())
                .expect("open ports")
                .get("sales")
                .expect("source port written")
                .version,
            1
        );
    }

    #[tokio::test]
    async fn stale_version_rebuilds_snapshot_and_retries_current_cell_source() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![cell(
            "a",
            "old_source()",
            1,
            dag(vec![port("a")], vec![], Some(source("csv", "sales"))),
        )]));
        let runner = FakeRunner::default();
        runner.fail_once("a");
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().to_path_buf(),
        );

        store
            .apply(jute::notebook_store::NotebookOp::WriteCell {
                id: "a".to_string(),
                source: "new_source()".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("human".to_string()),
            })
            .expect("human edit");

        engine
            .process_source_push(SourcePush {
                source: source("csv", "sales"),
                ipc_bytes: ipc_bytes(),
            })
            .await
            .expect("source push");

        let requests = runner.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].expected_version, 2);
        assert_eq!(requests[1].expected_version, 2);
        assert_eq!(requests[1].code, "new_source()");
    }

    #[test]
    fn debounce_keeps_latest_push_per_source_and_honors_in_flight_cap() {
        let mut debounce = SourceDebounce::new(ReactiveEngineConfig {
            source_debounce: std::time::Duration::from_millis(25),
            max_in_flight: 2,
        });

        debounce.push(SourcePush {
            source: source("csv", "sales"),
            ipc_bytes: vec![1],
        });
        debounce.push(SourcePush {
            source: source("csv", "sales"),
            ipc_bytes: vec![2],
        });

        assert_eq!(debounce.pending_len(), 1);
        assert_eq!(debounce.drain_ready(1).len(), 1);
        assert_eq!(debounce.available_slots(2), 0);
    }

    fn store_with_notebook(root: NotebookRoot) -> Arc<NotebookStore> {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load("/tmp/reactive.ipynb", root);
        store
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

    fn cell(id: &str, code: &str, version: u64, dag: CellDagMetadata) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version,
                    last_edited_by: None,
                    datasource_setup: None,
                    dag: Some(dag),
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single(code.to_string()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn dag(
        produces: Vec<PortSpec>,
        consumes: Vec<&str>,
        source: Option<DagSource>,
    ) -> CellDagMetadata {
        CellDagMetadata {
            produces,
            consumes: consumes.into_iter().map(str::to_string).collect(),
            source,
        }
    }

    fn port(port: &str) -> PortSpec {
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
            let mut writer = arrow_ipc::writer::FileWriter::try_new(&mut bytes, schema.as_ref())
                .expect("writer");
            writer.write(&batch).expect("write batch");
            writer.finish().expect("finish");
        }
        bytes
    }
}
