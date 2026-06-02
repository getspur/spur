use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use jute::{
    backend::notebook::{
        code_type_for_spec, kernelspec_for, Cell, CellDagMetadata, CodeType, DagSource,
        NotebookRoot,
    },
    commands::kernel_slot_info_for_state,
    notebook_store::{DeltaKind, NotebookDelta, NotebookStore},
    state::{slot_id_for, slot_id_for_spec},
};
use serde_json::{json, Value};
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use tracing::{debug, warn};

use crate::mcp::{
    tools::{run_cell, start_kernel},
    ServerDeps,
};

use super::{notebook_port_root, NotebookDag, PortStore, PortStoreError};

const DEFAULT_SOURCE_DEBOUNCE: Duration = Duration::from_millis(150);
const DEFAULT_MAX_IN_FLIGHT: usize = 4;
const STALE_RETRY_LIMIT: usize = 3;
const SUPPORTED_KERNELSPECS: [&str; 4] = ["python3", "deno", "evcxr", "gonb"];

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
    pub notebook_path: String,
    pub kernel_id: Option<String>,
    pub code: String,
    pub expected_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelEnsureRequest {
    pub slot_id: String,
    pub spec_name: String,
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

    fn ensure_kernel<'a>(
        &'a self,
        request: KernelEnsureRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>>;
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
                    "notebook_path": request.notebook_path,
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

    fn ensure_kernel<'a>(
        &'a self,
        request: KernelEnsureRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(state) = self.deps.state.as_ref() {
                if kernel_slot_info_for_state(&request.slot_id, state)
                    .await
                    .is_ok_and(|info| info.status != "dead" && info.spec_name == request.spec_name)
                {
                    return Ok(());
                }
            }

            start_kernel::call(
                &self.deps,
                json!({
                    "spec_name": request.spec_name,
                    "slot_id": request.slot_id,
                }),
            )
            .await
            .map(|_result| ())
            .map_err(|error| EngineError::KernelEnsure(format!("{error:?}")))
        })
    }
}

#[derive(Debug)]
pub enum EngineError {
    Dag(crate::dag::DagError),
    Port(PortStoreError),
    CellNotFound {
        cell_id: String,
    },
    MissingCellVersion {
        cell_id: String,
    },
    StaleCell {
        cell_id: String,
    },
    UnsupportedKernelspec {
        spec_name: String,
        cell_ids: Vec<String>,
    },
    KernelEnsure(String),
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
            Self::UnsupportedKernelspec {
                spec_name,
                cell_ids,
            } => write!(
                f,
                "kernelspec {spec_name} is not supported yet for cells: {}",
                cell_ids.join(", ")
            ),
            Self::KernelEnsure(error) => write!(f, "kernel ensure failed: {error}"),
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
    notebook_path: String,
    port_root: PathBuf,
    graph: Option<NotebookDag>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KernelRequirement {
    spec_name: String,
    slot_id: String,
    cell_ids: Vec<String>,
}

impl<R> ReactiveEngine<R>
where
    R: CellRunner,
{
    pub fn new(
        store: Arc<NotebookStore>,
        runner: R,
        notebook_path: impl AsRef<Path>,
        port_root: PathBuf,
    ) -> Self {
        let notebook_path = notebook_path.as_ref().to_string_lossy().into_owned();
        Self {
            store,
            runner,
            notebook_path,
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
        self.ensure_dag_kernels().await?;

        let mut ports = PortStore::open_at(&self.port_root)?;
        ports.put(&push.source.port, &push.ipc_bytes)?;

        let graph = self.graph.as_ref().expect("graph was rebuilt");
        let stale = graph.stale_from_source(&push.source)?;
        self.emit_dag_status_changed(BTreeMap::new())?;
        self.cascade_from(stale).await
    }

    pub async fn run_cell_and_cascade(
        &mut self,
        cell_id: &str,
    ) -> Result<CascadeReport, EngineError> {
        self.rebuild_graph()?;
        self.ensure_dag_kernels().await?;

        let port_versions = self.produced_port_versions(cell_id)?;
        let outcome = self.run_cell_with_status_events(cell_id).await?;
        let status = outcome.status;
        let mut report = CascadeReport {
            runs: vec![CellRunReport::new(cell_id, status)],
        };

        if status == CellRunStatus::Succeeded {
            self.bump_produced_ports_if_unchanged(cell_id, &port_versions)?;
            self.emit_run_report(cell_id, status)?;
            let stale = self.downstream_of(cell_id)?;
            report
                .runs
                .extend(self.cascade_from_ordered(stale).await?.runs);
        } else {
            self.emit_run_report(cell_id, status)?;
        }

        Ok(report)
    }

    pub async fn run_cell(&mut self, cell_id: &str) -> Result<CellRunReport, EngineError> {
        self.rebuild_graph()?;
        self.ensure_cell_kernel(cell_id).await?;

        let port_versions = self.produced_port_versions(cell_id)?;
        let outcome = self.run_cell_with_status_events(cell_id).await?;
        let status = outcome.status;
        let report = CellRunReport::new(cell_id, status);

        if status == CellRunStatus::Succeeded {
            self.bump_produced_ports_if_unchanged(cell_id, &port_versions)?;
            let mut states = self
                .downstream_of(cell_id)?
                .into_iter()
                .map(|downstream| (downstream, "stale"))
                .collect::<BTreeMap<_, _>>();
            states.insert(cell_id.to_owned(), CellRunStatus::Succeeded.as_dag_state());
            self.emit_dag_status_changed(states)?;
        } else {
            self.emit_run_report(cell_id, status)?;
        }

        Ok(report)
    }

    async fn cascade_from(&mut self, seeds: Vec<String>) -> Result<CascadeReport, EngineError> {
        let mut blocked = BTreeSet::new();
        let mut report = CascadeReport::default();

        for cell_id in seeds {
            if blocked.contains(&cell_id) {
                report.runs.push(CellRunReport::new(
                    cell_id.clone(),
                    CellRunStatus::UpstreamFailed,
                ));
                self.emit_run_report(&cell_id, CellRunStatus::UpstreamFailed)?;
                blocked.extend(self.downstream_of(&cell_id)?);
                continue;
            }

            let outcome = self.run_cell_with_status_events(&cell_id).await?;
            let status = outcome.status;
            report
                .runs
                .push(CellRunReport::new(cell_id.clone(), status));
            self.emit_run_report(&cell_id, status)?;
            if status == CellRunStatus::Failed {
                blocked.extend(self.downstream_of(&cell_id)?);
            }
        }

        Ok(report)
    }

    async fn cascade_from_ordered(
        &mut self,
        seeds: BTreeSet<String>,
    ) -> Result<CascadeReport, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(CascadeReport::default());
        };
        let ordered = graph
            .topological_sort()?
            .into_iter()
            .filter(|cell_id| seeds.contains(cell_id))
            .collect();
        self.cascade_from(ordered).await
    }

    async fn ensure_dag_kernels(&self) -> Result<(), EngineError> {
        let requirements = self.dag_kernel_requirements();
        self.ensure_kernel_requirements(requirements).await
    }

    async fn ensure_cell_kernel(&self, cell_id: &str) -> Result<(), EngineError> {
        let requirement = self.cell_kernel_requirement(cell_id)?;
        self.ensure_kernel_requirements(vec![requirement]).await
    }

    async fn ensure_kernel_requirements(
        &self,
        requirements: Vec<KernelRequirement>,
    ) -> Result<(), EngineError> {
        reject_unsupported_kernel_specs(&requirements)?;

        let mut join_set = JoinSet::new();
        for requirement in requirements {
            let runner = self.runner.clone();
            join_set.spawn(async move {
                runner
                    .ensure_kernel(KernelEnsureRequest {
                        slot_id: requirement.slot_id,
                        spec_name: requirement.spec_name,
                    })
                    .await
            });
        }

        let mut first_error = None;
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(EngineError::KernelEnsure(format!(
                            "kernel ensure task failed: {error}"
                        )));
                    }
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn dag_kernel_requirements(&self) -> Vec<KernelRequirement> {
        let (root, _) = self.store.snapshot();
        let mut by_spec = BTreeMap::<String, BTreeSet<String>>::new();
        for cell in &root.cells {
            if cell_dag(cell).is_none() {
                continue;
            }
            let Some(cell_id) = cell_id(cell) else {
                continue;
            };
            let code_type = resolve_code_type(&root, cell_code_type(cell));
            let spec_name = kernelspec_for(code_type).to_owned();
            by_spec.entry(spec_name).or_default().insert(cell_id);
        }
        requirements_from_spec_map(&self.notebook_path, by_spec)
    }

    fn cell_kernel_requirement(&self, cell_id: &str) -> Result<KernelRequirement, EngineError> {
        let (root, _) = self.store.snapshot();
        let cell = cell_view(&root, cell_id).ok_or_else(|| EngineError::CellNotFound {
            cell_id: cell_id.to_owned(),
        })?;
        let code_type = resolve_code_type(&root, cell.code_type);
        let spec_name = kernelspec_for(code_type).to_owned();
        Ok(KernelRequirement {
            slot_id: slot_id_for_spec(&self.notebook_path, &spec_name),
            spec_name,
            cell_ids: vec![cell_id.to_owned()],
        })
    }

    async fn run_cell_with_status_events(
        &mut self,
        cell_id: &str,
    ) -> Result<CellRunOutcome, EngineError> {
        let mut states = BTreeMap::new();
        states.insert(cell_id.to_owned(), "running");
        self.emit_dag_status_changed(states)?;
        self.run_cell_with_retries(cell_id).await
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

    fn emit_run_report(&self, cell_id: &str, status: CellRunStatus) -> Result<(), EngineError> {
        let mut states = BTreeMap::new();
        states.insert(cell_id.to_owned(), status.as_dag_state());
        self.emit_dag_status_changed(states)
    }

    fn emit_dag_status_changed(
        &self,
        states: BTreeMap<String, &'static str>,
    ) -> Result<(), EngineError> {
        let (root, version) = self.store.snapshot();
        let port_manifest = PortStore::open_read_only_at(&self.port_root)?
            .manifest()
            .iter()
            .map(|(port, entry)| (port.clone(), entry.version))
            .collect::<BTreeMap<_, _>>();
        self.store
            .publish_dag_status_changed(build_dag_status_snapshot(
                &root,
                version,
                &states,
                port_manifest,
            ));
        Ok(())
    }

    fn produced_port_versions(
        &self,
        cell_id: &str,
    ) -> Result<BTreeMap<String, Option<u64>>, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(BTreeMap::new());
        };
        let Some(metadata) = graph.cell_metadata(cell_id) else {
            return Ok(BTreeMap::new());
        };
        let store = PortStore::open_read_only_at(&self.port_root)?;
        Ok(metadata
            .produces
            .iter()
            .map(|produced| {
                (
                    produced.port.clone(),
                    store
                        .manifest()
                        .get(&produced.port)
                        .map(|entry| entry.version),
                )
            })
            .collect())
    }

    fn bump_produced_ports_if_unchanged(
        &self,
        cell_id: &str,
        before_versions: &BTreeMap<String, Option<u64>>,
    ) -> Result<bool, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(false);
        };
        let Some(metadata) = graph.cell_metadata(cell_id) else {
            return Ok(false);
        };

        let mut store = PortStore::open_at(&self.port_root)?;
        let mut changed = false;
        for produced in &metadata.produces {
            let port = produced.port.as_str();
            let read = match store.get(port) {
                Ok(read) => read,
                Err(PortStoreError::MissingPort(_)) => continue,
                Err(error) => return Err(error.into()),
            };
            let before = before_versions.get(port).copied().flatten();
            if Some(read.version) == before {
                // `ipc_bytes` is now an arrow Buffer (zero-copy mmap of the port
                // file); deref to &[u8] so it routes through PortPayload::IpcBytes.
                store.put(port, &*read.ipc_bytes)?;
                changed = true;
            }
        }
        Ok(changed)
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
        let code_type = resolve_code_type(&root, cell.code_type);
        Ok(CellRunRequest {
            cell_id: cell_id.to_owned(),
            notebook_path: self.notebook_path.clone(),
            kernel_id: Some(slot_id_for(&self.notebook_path, code_type)),
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

fn resolve_code_type(root: &NotebookRoot, code_type: Option<CodeType>) -> CodeType {
    code_type
        .or_else(|| {
            root.metadata
                .kernelspec
                .as_ref()
                .and_then(|kernelspec| code_type_for_spec(&kernelspec.name))
        })
        .unwrap_or(CodeType::Python)
}

fn requirements_from_spec_map(
    notebook_path: &str,
    by_spec: BTreeMap<String, BTreeSet<String>>,
) -> Vec<KernelRequirement> {
    by_spec
        .into_iter()
        .map(|(spec_name, cell_ids)| KernelRequirement {
            slot_id: slot_id_for_spec(notebook_path, &spec_name),
            spec_name,
            cell_ids: cell_ids.into_iter().collect(),
        })
        .collect()
}

fn reject_unsupported_kernel_specs(requirements: &[KernelRequirement]) -> Result<(), EngineError> {
    if let Some(requirement) = requirements
        .iter()
        .find(|requirement| !SUPPORTED_KERNELSPECS.contains(&requirement.spec_name.as_str()))
    {
        Err(EngineError::UnsupportedKernelspec {
            spec_name: requirement.spec_name.clone(),
            cell_ids: requirement.cell_ids.clone(),
        })
    } else {
        Ok(())
    }
}

impl CellRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::UpstreamFailed => "upstream_failed",
        }
    }

    fn as_dag_state(self) -> &'static str {
        match self {
            Self::Succeeded => "fresh",
            Self::Failed => "failed",
            Self::UpstreamFailed => "upstream-failed",
        }
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

    #[doc(hidden)]
    pub fn new_for_test(source_tx: mpsc::Sender<SourcePush>) -> Self {
        Self::new(source_tx)
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
                        let port_root = notebook_port_root(&path);
                        let complete_tx = complete_tx.clone();
                        tokio::spawn(async move {
                            let mut engine = ReactiveEngine::new(store, runner, &path, port_root);
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
    code_type: Option<CodeType>,
}

fn cell_view(root: &NotebookRoot, target: &str) -> Option<CellView> {
    root.cells.iter().find_map(|cell| {
        (cell_id(cell).as_deref() == Some(target)).then(|| CellView {
            source: cell_source(cell),
            version: cell_version(cell),
            code_type: cell_code_type(cell),
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

fn cell_code_type(cell: &Cell) -> Option<CodeType> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().and_then(|spur| spur.code_type),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().and_then(|spur| spur.code_type),
        Cell::Code(cell) => cell.metadata.spur.as_ref().and_then(|spur| spur.code_type),
    }
}

fn cell_source(cell: &Cell) -> String {
    match cell {
        Cell::Raw(cell) => String::from(cell.source.clone()),
        Cell::Markdown(cell) => String::from(cell.source.clone()),
        Cell::Code(cell) => String::from(cell.source.clone()),
    }
}

fn cell_execution_count(cell: &Cell) -> Option<u32> {
    match cell {
        Cell::Code(cell) => cell.execution_count,
        Cell::Raw(_) | Cell::Markdown(_) => None,
    }
}

fn build_dag_status_snapshot(
    root: &NotebookRoot,
    notebook_version: u64,
    states: &BTreeMap<String, &'static str>,
    port_manifest: BTreeMap<String, u64>,
) -> Value {
    let nodes = root
        .cells
        .iter()
        .filter_map(|cell| {
            let id = cell_id(cell)?;
            let state = states.get(&id).copied()?;
            Some(json!({
                "id": id,
                "state": state,
                "execution_count": cell_execution_count(cell),
            }))
        })
        .collect::<Vec<Value>>();

    json!({
        "notebook_version": notebook_version,
        "nodes": nodes,
        "port_manifest": port_manifest,
    })
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
            Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, DagSource, KernelSpec,
            MultilineString, NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        commands::SaveCoordinator,
        notebook_store::NotebookStore,
        state::State,
    };
    use tempfile::TempDir;

    use crate::mcp::{
        bridge::{AgentBridge, TauriBridgeRequester},
        ServerDeps,
    };

    #[derive(Clone, Default)]
    struct FakeRunner {
        requests: Arc<Mutex<Vec<CellRunRequest>>>,
        ensures: Arc<Mutex<Vec<KernelEnsureRequest>>>,
        active_ensures: Arc<Mutex<usize>>,
        max_active_ensures: Arc<Mutex<usize>>,
        events: Arc<Mutex<Vec<String>>>,
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

        fn ensures(&self) -> Vec<KernelEnsureRequest> {
            self.ensures.lock().expect("ensures lock").clone()
        }

        fn max_active_ensures(&self) -> usize {
            *self
                .max_active_ensures
                .lock()
                .expect("max active ensures lock")
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().expect("events lock").clone()
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
                self.events
                    .lock()
                    .expect("events lock")
                    .push(format!("run:{}", request.cell_id));
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

        fn ensure_kernel<'a>(
            &'a self,
            request: KernelEnsureRequest,
        ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>> {
            Box::pin(async move {
                self.ensures
                    .lock()
                    .expect("ensures lock")
                    .push(request.clone());
                self.events
                    .lock()
                    .expect("events lock")
                    .push(format!("ensure-start:{}", request.spec_name));
                {
                    let mut active = self.active_ensures.lock().expect("active ensures lock");
                    *active += 1;
                    let mut max_active = self
                        .max_active_ensures
                        .lock()
                        .expect("max active ensures lock");
                    *max_active = (*max_active).max(*active);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                {
                    let mut active = self.active_ensures.lock().expect("active ensures lock");
                    *active -= 1;
                }
                self.events
                    .lock()
                    .expect("events lock")
                    .push(format!("ensure-end:{}", request.spec_name));
                Ok(())
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
            temp.path().join("reactive.ipynb"),
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
            temp.path().join("reactive.ipynb"),
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

    #[tokio::test]
    async fn run_cell_and_cascade_runs_target_then_downstream_and_bumps_ports() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![port("b")], vec!["a"], None)),
            cell("c", "c = b", 1, dag(vec![], vec!["b"], None)),
            cell("z", "z = 1", 1, dag(vec![port("z")], vec![], None)),
        ]));
        let runner = FakeRunner::default();
        let mut ports = PortStore::open_at(temp.path()).expect("open ports");
        ports.put("a", &ipc_bytes()).expect("seed a");
        ports.put("b", &ipc_bytes()).expect("seed b");
        ports.put("z", &ipc_bytes()).expect("seed z");
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let report = engine.run_cell_and_cascade("a").await.expect("run cascade");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("a", CellRunStatus::Succeeded),
                CellRunReport::new("b", CellRunStatus::Succeeded),
                CellRunReport::new("c", CellRunStatus::Succeeded),
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
        let ports = PortStore::open_at(temp.path()).expect("open ports");
        assert_eq!(ports.get("a").expect("a bumped").version, 2);
        assert_eq!(ports.get("b").expect("b unchanged").version, 1);
        assert_eq!(ports.get("z").expect("z untouched").version, 1);
    }

    #[tokio::test]
    async fn run_cell_and_cascade_preflights_distinct_kernels_before_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![], vec!["a"], None)),
            cell("c", "console.log(a)", 1, dag(vec![], vec!["a"], None)),
        ]);
        set_code_type(&mut root, "c", CodeType::Javascript);
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        engine.run_cell_and_cascade("a").await.expect("run cascade");

        let expected_base = jute::state::notebook_slot_id(notebook_path.to_string_lossy().as_ref());
        assert_eq!(
            runner
                .ensures()
                .iter()
                .map(|request| (request.spec_name.clone(), request.slot_id.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("deno".to_string(), format!("{expected_base}#deno")),
                ("python3".to_string(), format!("{expected_base}#python3")),
            ]
        );
        assert!(
            runner.max_active_ensures() > 1,
            "kernel ensures should run concurrently"
        );
        let events = runner.events();
        let first_run = events
            .iter()
            .position(|event| event.starts_with("run:"))
            .expect("run event");
        assert!(events[..first_run]
            .iter()
            .any(|event| event == "ensure-end:deno"));
        assert!(events[..first_run]
            .iter()
            .any(|event| event == "ensure-end:python3"));
    }

    #[tokio::test]
    async fn run_cell_and_cascade_preflights_rust_cells_before_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("rs", "let x = 1;", 1, dag(vec![], vec!["a"], None)),
        ]);
        set_code_type(&mut root, "rs", CodeType::Rust);
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        let report = engine
            .run_cell_and_cascade("a")
            .await
            .expect("rust kernelspec is supported");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("a", CellRunStatus::Succeeded),
                CellRunReport::new("rs", CellRunStatus::Succeeded),
            ]
        );
        assert_eq!(
            runner
                .ensures()
                .iter()
                .map(|request| request.spec_name.as_str())
                .collect::<Vec<_>>(),
            vec!["evcxr", "python3"]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "rs"]
        );
    }

    #[tokio::test]
    async fn run_cell_and_cascade_preflights_go_cells_before_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("go", "x := 1", 1, dag(vec![], vec!["a"], None)),
        ]);
        set_code_type(&mut root, "go", CodeType::Go);
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        let report = engine
            .run_cell_and_cascade("a")
            .await
            .expect("go kernelspec is supported");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("a", CellRunStatus::Succeeded),
                CellRunReport::new("go", CellRunStatus::Succeeded),
            ]
        );
        assert_eq!(
            runner
                .ensures()
                .iter()
                .map(|request| request.spec_name.as_str())
                .collect::<Vec<_>>(),
            vec!["gonb", "python3"]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "go"]
        );
    }

    #[test]
    fn reject_unsupported_kernel_specs_rejects_unknown_specs() {
        let error = reject_unsupported_kernel_specs(&[KernelRequirement {
            spec_name: "ruby".to_string(),
            slot_id: "notebook#ruby".to_string(),
            cell_ids: vec!["rb".to_string()],
        }])
        .expect_err("unknown kernelspec is unsupported");

        assert!(matches!(
            error,
            EngineError::UnsupportedKernelspec {
                ref spec_name,
                ref cell_ids
            } if spec_name == "ruby" && cell_ids == &vec!["rb".to_string()]
        ));
    }

    #[tokio::test]
    async fn run_cell_lazily_preflights_only_target_kernel_before_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![
            cell("py", "py = 1", 1, dag(vec![], vec![], None)),
            cell("js", "console.log(1)", 1, dag(vec![], vec![], None)),
        ]);
        set_code_type(&mut root, "js", CodeType::Javascript);
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        engine.run_cell("js").await.expect("run cell");

        assert_eq!(
            runner
                .ensures()
                .iter()
                .map(|request| request.spec_name.as_str())
                .collect::<Vec<_>>(),
            vec!["deno"]
        );
        let events = runner.events();
        let ensure_end = events
            .iter()
            .position(|event| event == "ensure-end:deno")
            .expect("ensure completed");
        let run = events
            .iter()
            .position(|event| event == "run:js")
            .expect("run event");
        assert!(ensure_end < run);
    }

    #[tokio::test]
    async fn run_cell_and_cascade_blocks_descendants_after_downstream_failure() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![port("b")], vec!["a"], None)),
            cell("c", "c = b", 1, dag(vec![], vec!["b"], None)),
        ]));
        let runner = FakeRunner::default();
        runner.fail_run("b");
        let mut ports = PortStore::open_at(temp.path()).expect("open ports");
        ports.put("a", &ipc_bytes()).expect("seed a");
        ports.put("b", &ipc_bytes()).expect("seed b");
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let report = engine.run_cell_and_cascade("a").await.expect("run cascade");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("a", CellRunStatus::Succeeded),
                CellRunReport::new("b", CellRunStatus::Failed),
                CellRunReport::new("c", CellRunStatus::UpstreamFailed),
            ]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[tokio::test]
    async fn run_cell_marks_target_fresh_and_downstream_stale_without_cascade() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![port("b")], vec!["a"], None)),
            cell("c", "c = b", 1, dag(vec![], vec!["b"], None)),
        ]));
        let runner = FakeRunner::default();
        let mut ports = PortStore::open_at(temp.path()).expect("open ports");
        ports.put("a", &ipc_bytes()).expect("seed a");
        ports.put("b", &ipc_bytes()).expect("seed b");
        let mut rx = store.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let report = engine.run_cell("a").await.expect("run cell");

        assert_eq!(report, CellRunReport::new("a", CellRunStatus::Succeeded));
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );

        let mut final_snapshot = None;
        while let Ok(delta) = rx.try_recv() {
            if let DeltaKind::DagStatusChanged { snapshot } = delta.kind {
                final_snapshot = Some(snapshot);
            }
        }
        let snapshot = final_snapshot.expect("final dag status snapshot");
        assert_eq!(
            snapshot["nodes"],
            json!([
                { "id": "a", "state": "fresh", "execution_count": null },
                { "id": "b", "state": "stale", "execution_count": null },
                { "id": "c", "state": "stale", "execution_count": null },
            ])
        );
    }

    #[tokio::test]
    async fn run_cell_request_uses_notebook_path_slot_id() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let expected_slot = jute::state::notebook_slot_id(notebook_path.to_string_lossy().as_ref());
        let expected_slot = format!("{expected_slot}#python3");
        let store = store_with_notebook(notebook(vec![cell(
            "a",
            "a = 1",
            1,
            dag(vec![], vec![], None),
        )]));
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        engine.run_cell("a").await.expect("run cell");

        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].notebook_path,
            notebook_path.to_string_lossy().as_ref()
        );
        assert_eq!(requests[0].kernel_id, Some(expected_slot));
    }

    #[tokio::test]
    async fn run_cell_request_uses_explicit_cell_code_type_slot_id() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![cell("a", "a = 1", 1, dag(vec![], vec![], None))]);
        if let Cell::Code(cell) = &mut root.cells[0] {
            cell.metadata
                .spur
                .as_mut()
                .expect("spur metadata")
                .code_type = Some(CodeType::Javascript);
        }
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        engine.run_cell("a").await.expect("run cell");

        let requests = runner.requests();
        let expected_slot = format!(
            "{}#deno",
            jute::state::notebook_slot_id(notebook_path.to_string_lossy().as_ref())
        );
        assert_eq!(requests[0].kernel_id, Some(expected_slot));
    }

    #[tokio::test]
    async fn run_cell_request_falls_back_to_notebook_kernelspec_slot_id() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let mut root = notebook(vec![cell("a", "a = 1", 1, dag(vec![], vec![], None))]);
        root.metadata.kernelspec = Some(KernelSpec {
            name: "deno".to_string(),
            display_name: "Deno".to_string(),
            other: Default::default(),
        });
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            &notebook_path,
            temp.path().join("ports"),
        );

        engine.run_cell("a").await.expect("run cell");

        let requests = runner.requests();
        let expected_slot = format!(
            "{}#deno",
            jute::state::notebook_slot_id(notebook_path.to_string_lossy().as_ref())
        );
        assert_eq!(requests[0].kernel_id, Some(expected_slot));
    }

    #[tokio::test]
    async fn daemonless_command_runner_lazily_ensures_explicit_kernel_id() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ui.ipynb");
        let root = notebook(vec![cell("a", "a = 1", 1, dag(vec![], vec![], None))]);
        let state = Arc::new(State::new());
        state.get_notebook().load(notebook_path.clone(), root);
        let deps = ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(Arc::clone(&state)),
            app: None,
            daemon: None,
        };
        let runner = RunCellCommandRunner::new(Arc::new(deps));
        let mut engine = ReactiveEngine::new(
            state.get_notebook(),
            runner,
            notebook_path,
            temp.path().join("ports"),
        );

        let error = engine
            .run_cell("a")
            .await
            .expect_err("test runner cannot provision python without an app handle");
        let message = error.to_string();

        assert!(
            !message.contains("requires kernel_id when no notebook is open"),
            "{message}"
        );
        assert!(message.contains("notebook.start_kernel requires a Tauri app handle"));
    }

    #[test]
    fn dag_status_snapshot_is_partial_and_uses_frontend_state_vocabulary() {
        let root = notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![], vec!["a"], None)),
            cell("c", "c = 1", 1, dag(vec![], vec![], None)),
        ]);
        let states = BTreeMap::from([
            ("a".to_string(), "fresh"),
            ("b".to_string(), "upstream-failed"),
        ]);
        let snapshot =
            build_dag_status_snapshot(&root, 42, &states, BTreeMap::from([("a".to_string(), 7)]));

        assert_eq!(snapshot["notebook_version"], 42);
        assert_eq!(snapshot["port_manifest"], json!({ "a": 7 }));
        assert_eq!(
            snapshot["nodes"],
            json!([
                { "id": "a", "state": "fresh", "execution_count": null },
                { "id": "b", "state": "upstream-failed", "execution_count": null },
            ])
        );
        let encoded = serde_json::to_string(&snapshot).expect("encode snapshot");
        assert!(!encoded.contains("idle"));
        assert!(!encoded.contains("succeeded"));
        assert!(!encoded.contains("upstream_failed"));
        assert!(!encoded.contains("\"c\""));
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
                    code_type: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single(code.to_string()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn set_code_type(root: &mut NotebookRoot, id: &str, code_type: CodeType) {
        for cell in &mut root.cells {
            if let Cell::Code(cell) = cell {
                if cell.id.as_deref() == Some(id) {
                    cell.metadata
                        .spur
                        .as_mut()
                        .expect("spur metadata")
                        .code_type = Some(code_type);
                    return;
                }
            }
        }
        panic!("missing cell {id}");
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
