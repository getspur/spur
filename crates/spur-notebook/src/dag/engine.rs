use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
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
    sync::{broadcast, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use tracing::{debug, warn};

use crate::mcp::{
    tools::{run_cell, start_kernel},
    ServerDeps,
};

use super::{
    notebook_port_root, validate_declared_schema, CascadeStatus, DeclaredSchemaError, NotebookDag,
    Origin, PortClass, PortEntry, PortEvent, PortEventClient, PortEventDraft, PortEventError,
    PortEventKind, PortEventSequencer, PortEventSequencerConfig, PortKind, PortPayload, PortRef,
    PortStore, PortStoreError, RunInput, RunStatus,
};

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
    pub payload: SourcePayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourcePayload {
    IpcBytes(Vec<u8>),
    MediaBlob {
        bytes: Vec<u8>,
        mime: String,
        /// Duration of the media content in seconds, if known. `None` means
        /// the duration was not supplied by the producer; consumers that need a
        /// concrete value should fall back to their own default.
        duration_sec: Option<f64>,
    },
}

// Manual Eq impl required because f64 does not implement Eq. The implementation
// uses total equality semantics: two NaN values compare equal, matching
// the intent of "same payload" checks in tests.
impl Eq for SourcePayload {}

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
    Stale,
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

#[derive(Clone)]
struct EventSink {
    draft_tx: mpsc::Sender<PortEventDraft>,
    next_cascade_id: u64,
}

struct ActiveCascade {
    cascade_id: u64,
    next_run_id: u64,
    active_runs: BTreeMap<String, u64>,
    events: Vec<PortEventKind>,
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
    DeclaredSchema(DeclaredSchemaError),
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
            Self::DeclaredSchema(error) => write!(f, "{error}"),
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

impl EngineError {
    fn event_code(&self) -> &'static str {
        match self {
            Self::Dag(_) => "dag",
            Self::Port(_) => "port",
            Self::DeclaredSchema(_) => "declared_schema",
            Self::CellNotFound { .. } => "cell_not_found",
            Self::MissingCellVersion { .. } => "missing_cell_version",
            Self::StaleCell { .. } => "stale_cell",
            Self::UnsupportedKernelspec { .. } => "unsupported_kernelspec",
            Self::KernelEnsure(_) => "kernel_ensure",
            Self::RunCell(_) => "run_cell",
            Self::DaemonUnavailable => "daemon_unavailable",
            Self::SourceQueueClosed => "source_queue_closed",
        }
    }
}

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

impl From<DeclaredSchemaError> for EngineError {
    fn from(error: DeclaredSchemaError) -> Self {
        Self::DeclaredSchema(error)
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
    event_sink: Option<EventSink>,
    active_cascade: Option<ActiveCascade>,
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
            event_sink: None,
            active_cascade: None,
        }
    }

    pub fn with_event_draft_sender(self, draft_tx: mpsc::Sender<PortEventDraft>) -> Self {
        self.with_event_draft_sender_and_cascade_id(draft_tx, 1)
    }

    fn with_event_draft_sender_and_cascade_id(
        mut self,
        draft_tx: mpsc::Sender<PortEventDraft>,
        cascade_id: u64,
    ) -> Self {
        self.event_sink = Some(EventSink {
            draft_tx,
            next_cascade_id: cascade_id,
        });
        self
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
        self.begin_source_cascade().await;
        let source_port = push.source.port.clone();
        let result = self.process_source_push_inner(push).await;
        match result {
            Ok(report) => {
                self.finish_cascade(CascadeStatus::Succeeded).await;
                self.active_cascade = None;
                Ok(report)
            }
            Err(error) => {
                self.emit_cascade_error(&error, Some(source_port)).await;
                self.finish_cascade(CascadeStatus::Failed).await;
                self.active_cascade = None;
                Err(error)
            }
        }
    }

    async fn process_source_push_inner(
        &mut self,
        push: SourcePush,
    ) -> Result<CascadeReport, EngineError> {
        self.rebuild_graph()?;
        self.ensure_dag_kernels().await?;

        let mut ports = PortStore::open_at(&self.port_root)?;
        match &push.payload {
            SourcePayload::IpcBytes(bytes) => {
                if let Some(declared) = push.source.schema.as_ref() {
                    let (schema, _) = super::ports::read_ipc_for_validation(bytes)?;
                    validate_declared_schema(&push.source.port, declared, schema.as_ref())?;
                }
                ports.put(&push.source.port, bytes)?;
            }
            SourcePayload::MediaBlob {
                bytes,
                mime,
                duration_sec,
            } => {
                if push.source.schema.is_some() {
                    return Err(DeclaredSchemaError::InvalidSchema {
                        port: push.source.port.clone(),
                        message: "declared schemas apply only to dataframe ports".to_string(),
                    }
                    .into());
                }
                ports.put(
                    &push.source.port,
                    PortPayload::MediaBlob {
                        bytes,
                        mime: mime.as_str(),
                        duration_sec: *duration_sec,
                    },
                )?;
            }
        }

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
            let outputs = self.produced_port_output_refs(cell_id, &port_versions)?;
            self.emit_run_report(cell_id, status, outputs).await?;
            let stale = self.downstream_of(cell_id)?;
            report
                .runs
                .extend(self.cascade_from_ordered(stale).await?.runs);
        } else {
            self.emit_run_report(cell_id, status, Vec::new()).await?;
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
            self.emit_run_report(cell_id, status, Vec::new()).await?;
        }

        Ok(report)
    }

    async fn cascade_from(&mut self, seeds: Vec<String>) -> Result<CascadeReport, EngineError> {
        let mut blocked = BTreeMap::new();
        let mut report = CascadeReport::default();

        for cell_id in seeds {
            if let Some(status) = blocked.get(&cell_id).copied() {
                report
                    .runs
                    .push(CellRunReport::new(cell_id.clone(), status));
                self.emit_run_report(&cell_id, status, Vec::new()).await?;
                for downstream in self.downstream_of(&cell_id)? {
                    blocked.insert(downstream, status);
                }
                continue;
            }

            if self.should_mark_stale_on_cascade(&cell_id)? {
                report
                    .runs
                    .push(CellRunReport::new(cell_id.clone(), CellRunStatus::Stale));
                self.emit_run_report(&cell_id, CellRunStatus::Stale, Vec::new())
                    .await?;
                for downstream in self.downstream_of(&cell_id)? {
                    blocked.insert(downstream, CellRunStatus::Stale);
                }
                continue;
            }

            let port_versions = self.produced_port_versions(&cell_id)?;
            let outcome = self.run_cell_with_status_events(&cell_id).await?;
            let status = outcome.status;
            report
                .runs
                .push(CellRunReport::new(cell_id.clone(), status));
            let outputs = if status == CellRunStatus::Succeeded {
                self.bump_produced_ports_if_unchanged(&cell_id, &port_versions)?;
                self.produced_port_output_refs(&cell_id, &port_versions)?
            } else {
                Vec::new()
            };
            self.emit_run_report(&cell_id, status, outputs).await?;
            if status == CellRunStatus::Failed {
                for downstream in self.downstream_of(&cell_id)? {
                    blocked.insert(downstream, CellRunStatus::UpstreamFailed);
                }
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
        if self.active_cascade.is_some() {
            self.emit_run_started(cell_id).await?;
        } else {
            let mut states = BTreeMap::new();
            states.insert(cell_id.to_owned(), "running");
            self.emit_dag_status_changed(states)?;
        }
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

    async fn emit_run_report(
        &mut self,
        cell_id: &str,
        status: CellRunStatus,
        outputs: Vec<PortRef>,
    ) -> Result<(), EngineError> {
        if self.active_cascade.is_some() {
            self.emit_run_finished(cell_id, status, outputs).await?;
            Ok(())
        } else {
            let mut states = BTreeMap::new();
            states.insert(cell_id.to_owned(), status.as_dag_state());
            self.emit_dag_status_changed(states)
        }
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
        if let Some(cascade) = &self.active_cascade {
            self.store
                .publish_dag_status_changed(build_dag_status_snapshot_from_events(
                    &root,
                    version,
                    &cascade.events,
                    port_manifest,
                ));
            return Ok(());
        }
        self.store
            .publish_dag_status_changed(build_dag_status_snapshot(
                &root,
                version,
                &states,
                port_manifest,
            ));
        Ok(())
    }

    async fn begin_source_cascade(&mut self) {
        let Some(sink) = self.event_sink.as_mut() else {
            return;
        };
        let cascade_id = sink.next_cascade_id;
        sink.next_cascade_id = sink.next_cascade_id.saturating_add(1);
        self.active_cascade = Some(ActiveCascade {
            cascade_id,
            next_run_id: 1,
            active_runs: BTreeMap::new(),
            events: Vec::new(),
        });
        self.record_port_event(PortEventKind::CascadeStarted {
            cascade_id,
            trigger: Origin::Agent {
                tool: "reactive_engine".to_string(),
            },
        })
        .await;
    }

    async fn emit_run_started(&mut self, cell_id: &str) -> Result<(), EngineError> {
        let inputs = self.resolve_run_inputs(cell_id)?;
        let Some(cascade) = self.active_cascade.as_mut() else {
            return Ok(());
        };
        let run_id = cascade.next_run_id;
        cascade.next_run_id = cascade.next_run_id.saturating_add(1);
        cascade.active_runs.insert(cell_id.to_string(), run_id);
        let cascade_id = cascade.cascade_id;
        self.record_port_event(PortEventKind::RunStarted {
            cascade_id,
            run_id,
            cell_id: cell_id.to_string(),
            inputs,
        })
        .await;
        self.emit_dag_status_changed(BTreeMap::new())
    }

    async fn emit_run_finished(
        &mut self,
        cell_id: &str,
        status: CellRunStatus,
        outputs: Vec<PortRef>,
    ) -> Result<(), EngineError> {
        let Some(cascade) = self.active_cascade.as_mut() else {
            return Ok(());
        };
        let run_id = cascade.active_runs.remove(cell_id).unwrap_or_else(|| {
            let run_id = cascade.next_run_id;
            cascade.next_run_id = cascade.next_run_id.saturating_add(1);
            run_id
        });
        let cascade_id = cascade.cascade_id;
        self.record_port_event(PortEventKind::RunFinished {
            cascade_id,
            run_id,
            cell_id: cell_id.to_string(),
            status: status.as_run_status(),
            outputs,
        })
        .await;
        self.emit_dag_status_changed(BTreeMap::new())
    }

    async fn finish_cascade(&mut self, status: CascadeStatus) {
        let Some(cascade) = &self.active_cascade else {
            return;
        };
        self.record_port_event(PortEventKind::CascadeFinished {
            cascade_id: cascade.cascade_id,
            status,
        })
        .await;
    }

    async fn emit_cascade_error(&mut self, error: &EngineError, port: Option<String>) {
        let Some(cascade) = &self.active_cascade else {
            return;
        };
        self.record_port_event(PortEventKind::CascadeError {
            cascade_id: cascade.cascade_id,
            code: error.event_code().to_string(),
            message: error.to_string(),
            port,
        })
        .await;
    }

    async fn record_port_event(&mut self, kind: PortEventKind) {
        let Some(draft_tx) = self.event_sink.as_ref().map(|sink| sink.draft_tx.clone()) else {
            return;
        };
        if let Some(cascade) = self.active_cascade.as_mut() {
            cascade.events.push(kind.clone());
        }
        if draft_tx.send(PortEventDraft::new(kind)).await.is_err() {
            warn!("reactive cascade port event sink closed");
        }
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

    fn resolve_run_inputs(&self, cell_id: &str) -> Result<Vec<RunInput>, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(Vec::new());
        };
        let consumed_ports = graph.consumed_ports(cell_id);
        if consumed_ports.is_empty() {
            return Ok(Vec::new());
        }
        let store = PortStore::open_read_only_at(&self.port_root)?;
        Ok(consumed_ports
            .into_iter()
            .map(|port| RunInput {
                r#ref: store.manifest().get(&port).map(|entry| {
                    port_ref_from_entry(
                        &port,
                        entry,
                        graph
                            .declared_schema_for_port(&port)
                            .and_then(|schema| super::ports::schema_hash(schema).ok()),
                    )
                }),
                port,
            })
            .collect())
    }

    fn produced_port_output_refs(
        &self,
        cell_id: &str,
        before_versions: &BTreeMap<String, Option<u64>>,
    ) -> Result<Vec<PortRef>, EngineError> {
        let Some(graph) = &self.graph else {
            return Ok(Vec::new());
        };
        let Some(metadata) = graph.cell_metadata(cell_id) else {
            return Ok(Vec::new());
        };
        let store = PortStore::open_read_only_at(&self.port_root)?;
        metadata
            .produces
            .iter()
            .filter_map(|produced| {
                let port = produced.port.as_str();
                let entry = store.manifest().get(port)?;
                let before = before_versions.get(port).copied().flatten();
                (Some(entry.version) != before).then_some((port, entry))
            })
            .map(|(port, entry)| {
                let schema_hash = match graph.declared_schema_for_port(port) {
                    Some(declared) => {
                        match &entry.kind {
                            PortKind::Arrow(actual) => {
                                validate_declared_schema(port, declared, actual)?;
                            }
                            PortKind::Media { .. } => {
                                return Err(DeclaredSchemaError::InvalidSchema {
                                    port: port.to_owned(),
                                    message: "declared schemas apply only to dataframe ports"
                                        .to_string(),
                                }
                                .into());
                            }
                        }
                        Some(super::ports::schema_hash(declared).map_err(|source| {
                            DeclaredSchemaError::InvalidSchema {
                                port: port.to_owned(),
                                message: source.to_string(),
                            }
                        })?)
                    }
                    None => None,
                };
                Ok(port_ref_from_entry(port, entry, schema_hash))
            })
            .collect()
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
            let before = before_versions.get(port).copied().flatten();
            let current_version = match store.manifest().get(port) {
                Some(entry) => entry.version,
                None => continue,
            };
            if Some(current_version) == before {
                match store.bump_version(port) {
                    Ok(_) => {}
                    Err(PortStoreError::MissingPort(_)) => continue,
                    Err(error) => return Err(error.into()),
                }
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

    fn should_mark_stale_on_cascade(&self, cell_id: &str) -> Result<bool, EngineError> {
        let (root, _) = self.store.snapshot();
        let cell = cell_view(&root, cell_id).ok_or_else(|| EngineError::CellNotFound {
            cell_id: cell_id.to_owned(),
        })?;
        Ok(cell.kernelspec.as_deref() == Some("spur") && !cell.ai_live)
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
            Self::Stale => "stale",
        }
    }

    fn as_run_status(self) -> RunStatus {
        match self {
            Self::Succeeded => RunStatus::Succeeded,
            Self::Failed => RunStatus::Failed,
            Self::UpstreamFailed => RunStatus::UpstreamFailed,
            Self::Stale => RunStatus::Stale,
        }
    }

    fn as_dag_state(self) -> &'static str {
        match self {
            Self::Succeeded => "fresh",
            Self::Failed => "failed",
            Self::UpstreamFailed => "upstream-failed",
            Self::Stale => "stale",
        }
    }
}

pub struct ReactiveEngineHandle {
    source_tx: mpsc::Sender<SourcePush>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    event_sequencer: Option<PortEventSequencer>,
}

#[derive(Clone)]
pub struct ReactiveEngineClient {
    source_tx: mpsc::Sender<SourcePush>,
    port_events: Option<PortEventClient>,
}

impl ReactiveEngineClient {
    pub(crate) fn new(source_tx: mpsc::Sender<SourcePush>) -> Self {
        Self {
            source_tx,
            port_events: None,
        }
    }

    pub(crate) fn new_with_port_events(
        source_tx: mpsc::Sender<SourcePush>,
        port_events: PortEventClient,
    ) -> Self {
        Self {
            source_tx,
            port_events: Some(port_events),
        }
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

    pub async fn emit_port_event(&self, draft: PortEventDraft) -> Result<(), PortEventError> {
        let Some(port_events) = &self.port_events else {
            return Ok(());
        };
        port_events.emit(draft).await
    }

    pub fn subscribe_port_events(&self) -> Option<broadcast::Receiver<PortEvent>> {
        self.port_events.as_ref().map(PortEventClient::subscribe)
    }

    pub async fn recent_port_events(&self) -> Vec<PortEvent> {
        let Some(port_events) = &self.port_events else {
            return Vec::new();
        };
        port_events.recent_events().await
    }
}

impl ReactiveEngineHandle {
    pub fn client(&self) -> ReactiveEngineClient {
        let Some(sequencer) = &self.event_sequencer else {
            return ReactiveEngineClient::new(self.source_tx.clone());
        };
        ReactiveEngineClient::new_with_port_events(self.source_tx.clone(), sequencer.client())
    }

    pub fn port_event_client(&self) -> PortEventClient {
        self.event_sequencer
            .as_ref()
            .expect("reactive engine port event sequencer is not available after shutdown")
            .client()
    }

    pub fn subscribe_port_events(&self) -> broadcast::Receiver<PortEvent> {
        self.port_event_client().subscribe()
    }

    pub async fn recent_port_events(&self) -> Vec<PortEvent> {
        self.port_event_client().recent_events().await
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
        if let Some(sequencer) = self.event_sequencer.take() {
            sequencer.shutdown().await;
        }
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
    let event_sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
    let event_draft_tx = event_sequencer.client().draft_sender();
    let next_cascade_id = Arc::new(AtomicU64::new(1));

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
                        let store = state.notebook_for_path(&path);
                        let runner = runner.clone();
                        let port_root = notebook_port_root(&path);
                        let complete_tx = complete_tx.clone();
                        let event_draft_tx = event_draft_tx.clone();
                        let cascade_id = next_cascade_id.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            let mut engine = ReactiveEngine::new(store, runner, &path, port_root)
                                .with_event_draft_sender_and_cascade_id(
                                    event_draft_tx,
                                    cascade_id,
                                );
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
        event_sequencer: Some(event_sequencer),
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

fn port_ref_from_entry(port: &str, entry: &PortEntry, schema_hash: Option<String>) -> PortRef {
    PortRef {
        port: port.to_owned(),
        version: entry.version,
        class: match &entry.kind {
            PortKind::Arrow(_) => PortClass::Dataframe,
            PortKind::Media { .. } => PortClass::Media,
        },
        schema_hash,
    }
}

struct CellView {
    source: String,
    version: Option<u64>,
    code_type: Option<CodeType>,
    kernelspec: Option<String>,
    ai_live: bool,
}

fn cell_view(root: &NotebookRoot, target: &str) -> Option<CellView> {
    root.cells.iter().find_map(|cell| {
        (cell_id(cell).as_deref() == Some(target)).then(|| CellView {
            source: cell_source(cell),
            version: cell_version(cell),
            code_type: cell_code_type(cell),
            kernelspec: cell_kernelspec(cell),
            ai_live: cell_ai_live(cell),
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

fn cell_kernelspec(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => metadata_kernelspec(&cell.metadata),
        Cell::Markdown(cell) => metadata_kernelspec(&cell.metadata),
        Cell::Code(cell) => metadata_kernelspec(&cell.metadata),
    }
}

fn metadata_kernelspec(metadata: &jute::backend::notebook::CellMetadata) -> Option<String> {
    metadata.other.get("kernelspec").and_then(kernelspec_name)
}

fn kernelspec_name(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => Some(name.clone()),
        Value::Object(object) => object
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn cell_ai_live(cell: &Cell) -> bool {
    match cell {
        Cell::Raw(cell) => metadata_ai_live(&cell.metadata),
        Cell::Markdown(cell) => metadata_ai_live(&cell.metadata),
        Cell::Code(cell) => metadata_ai_live(&cell.metadata),
    }
}

fn metadata_ai_live(metadata: &jute::backend::notebook::CellMetadata) -> bool {
    metadata
        .other
        .get("ai_live")
        .and_then(Value::as_bool)
        .unwrap_or(false)
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

fn build_dag_status_snapshot_from_events(
    root: &NotebookRoot,
    notebook_version: u64,
    events: &[PortEventKind],
    port_manifest: BTreeMap<String, u64>,
) -> Value {
    let mut states = BTreeMap::new();
    for event in events {
        match event {
            PortEventKind::RunStarted { cell_id, .. } => {
                states.insert(cell_id.clone(), "running");
            }
            PortEventKind::RunFinished {
                cell_id, status, ..
            } => {
                states.insert(cell_id.clone(), status.as_dag_state());
            }
            PortEventKind::PortPut { .. }
            | PortEventKind::CascadeStarted { .. }
            | PortEventKind::CascadeFinished { .. }
            | PortEventKind::CascadeError { .. }
            | PortEventKind::IntentRejected { .. } => {}
        }
    }
    build_dag_status_snapshot(root, notebook_version, &states, port_manifest)
}

impl RunStatus {
    fn as_dag_state(self) -> &'static str {
        match self {
            Self::Succeeded | Self::SkippedFresh => "fresh",
            Self::Failed => "failed",
            Self::UpstreamFailed => "upstream-failed",
            Self::Stale => "stale",
        }
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
        writes: Arc<Mutex<BTreeMap<String, VecDeque<(PathBuf, String)>>>>,
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

        fn write_port_on_run(&self, cell_id: &str, port_root: &Path, port: &str) {
            self.writes
                .lock()
                .expect("writes lock")
                .entry(cell_id.to_string())
                .or_default()
                .push_back((port_root.to_path_buf(), port.to_string()));
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

    async fn recv_port_event_kinds(
        subscription: &mut tokio::sync::broadcast::Receiver<PortEvent>,
        count: usize,
    ) -> Vec<PortEventKind> {
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            let event = tokio::time::timeout(Duration::from_secs(1), subscription.recv())
                .await
                .expect("timed out waiting for port event")
                .expect("port event");
            events.push(event.into_kind());
        }
        events
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
                if let Some((port_root, port)) = self
                    .writes
                    .lock()
                    .expect("writes lock")
                    .get_mut(&request.cell_id)
                    .and_then(VecDeque::pop_front)
                {
                    PortStore::open_at(port_root)
                        .expect("open port store for fake kernel write")
                        .put(&port, &ipc_bytes())
                        .expect("fake kernel port write");
                }
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
                payload: SourcePayload::IpcBytes(ipc_bytes()),
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
                .version(),
            1
        );
    }

    #[tokio::test]
    async fn source_push_emits_cascade_port_event_drafts() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![
            cell(
                "a",
                "a = spur.get('sales')",
                1,
                dag(vec![port("a")], vec![], Some(source("csv", "sales"))),
            ),
            cell("b", "b = a", 1, dag(vec![], vec!["a"], None)),
        ]));
        let runner = FakeRunner::default();
        runner.write_port_on_run("a", temp.path(), "a");
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let event_client = sequencer.client();
        let mut subscription = event_client.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        )
        .with_event_draft_sender(event_client.draft_sender());

        engine
            .process_source_push(SourcePush {
                source: source("csv", "sales"),
                payload: SourcePayload::IpcBytes(ipc_bytes()),
            })
            .await
            .expect("source push");

        let events = recv_port_event_kinds(&mut subscription, 6).await;

        assert_eq!(
            events,
            vec![
                PortEventKind::CascadeStarted {
                    cascade_id: 1,
                    trigger: Origin::Agent {
                        tool: "reactive_engine".to_string(),
                    },
                },
                PortEventKind::RunStarted {
                    cascade_id: 1,
                    run_id: 1,
                    cell_id: "a".to_string(),
                    inputs: vec![],
                },
                PortEventKind::RunFinished {
                    cascade_id: 1,
                    run_id: 1,
                    cell_id: "a".to_string(),
                    status: RunStatus::Succeeded,
                    outputs: vec![PortRef {
                        port: "a".to_string(),
                        version: 1,
                        class: PortClass::Dataframe,
                        schema_hash: None,
                    }],
                },
                PortEventKind::RunStarted {
                    cascade_id: 1,
                    run_id: 2,
                    cell_id: "b".to_string(),
                    inputs: vec![RunInput {
                        port: "a".to_string(),
                        r#ref: Some(PortRef {
                            port: "a".to_string(),
                            version: 1,
                            class: PortClass::Dataframe,
                            schema_hash: None,
                        }),
                    }],
                },
                PortEventKind::RunFinished {
                    cascade_id: 1,
                    run_id: 2,
                    cell_id: "b".to_string(),
                    status: RunStatus::Succeeded,
                    outputs: vec![],
                },
                PortEventKind::CascadeFinished {
                    cascade_id: 1,
                    status: CascadeStatus::Succeeded,
                },
            ]
        );
        drop(engine);
        drop(subscription);
        drop(event_client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn source_push_rejects_declared_schema_mismatch_before_put() {
        let temp = TempDir::new().expect("temp dir");
        let source = source_with_schema("csv", "sales", declared_schema_json("Float64"));
        let store = store_with_notebook(notebook(vec![cell(
            "a",
            "a = spur.get('sales')",
            1,
            dag(vec![port("a")], vec![], Some(source.clone())),
        )]));
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let err = engine
            .process_source_push(SourcePush {
                source,
                payload: SourcePayload::IpcBytes(ipc_bytes()),
            })
            .await
            .expect_err("declared source schema mismatch fails");

        assert_eq!(
            err.to_string(),
            "port 'sales' field 'value': declared Float64, got Int64"
        );
        assert!(
            PortStore::open_at(temp.path())
                .expect("open ports")
                .manifest()
                .get("sales")
                .is_none(),
            "mismatched source push must not commit a port entry"
        );
    }

    #[tokio::test]
    async fn run_started_records_missing_consumed_port_before_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![cell(
            "consumer",
            "value = spur.get('missing')",
            1,
            dag(vec![], vec!["missing"], None),
        )]));
        let runner = FakeRunner::default();
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let event_client = sequencer.client();
        let mut subscription = event_client.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        )
        .with_event_draft_sender(event_client.draft_sender());

        engine.rebuild_graph().expect("graph");
        engine.begin_source_cascade().await;
        engine
            .run_cell_with_status_events("consumer")
            .await
            .expect("run consumer");
        engine
            .emit_run_report("consumer", CellRunStatus::Succeeded, Vec::new())
            .await
            .expect("finish run");

        let events = recv_port_event_kinds(&mut subscription, 3).await;
        assert!(matches!(
            &events[1],
            PortEventKind::RunStarted {
                cell_id,
                inputs,
                ..
            } if cell_id == "consumer" && inputs == &vec![RunInput {
                port: "missing".to_string(),
                r#ref: None,
            }]
        ));

        drop(engine);
        drop(subscription);
        drop(event_client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn run_finished_records_kernel_produced_port_output() {
        let temp = TempDir::new().expect("temp dir");
        let declared = declared_schema_json("Int64");
        let store = store_with_notebook(notebook(vec![cell(
            "producer",
            "spur.put('result', df)",
            1,
            dag(
                vec![port_with_schema("result", declared.clone())],
                vec![],
                None,
            ),
        )]));
        let runner = FakeRunner::default();
        runner.write_port_on_run("producer", temp.path(), "result");
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let event_client = sequencer.client();
        let mut subscription = event_client.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        )
        .with_event_draft_sender(event_client.draft_sender());

        engine.rebuild_graph().expect("graph");
        engine.begin_source_cascade().await;
        let before = engine
            .produced_port_versions("producer")
            .expect("before versions");
        engine
            .run_cell_with_status_events("producer")
            .await
            .expect("run producer");
        engine
            .bump_produced_ports_if_unchanged("producer", &before)
            .expect("bump unchanged");
        let outputs = engine
            .produced_port_output_refs("producer", &before)
            .expect("output refs");
        engine
            .emit_run_report("producer", CellRunStatus::Succeeded, outputs)
            .await
            .expect("finish run");

        let events = recv_port_event_kinds(&mut subscription, 3).await;
        assert!(matches!(
            &events[2],
            PortEventKind::RunFinished {
                cell_id,
                outputs,
                ..
            } if cell_id == "producer" && outputs == &vec![PortRef {
                port: "result".to_string(),
                version: 1,
                class: PortClass::Dataframe,
                schema_hash: Some(crate::dag::schema_hash(&declared).expect("schema hash")),
            }]
        ));

        drop(engine);
        drop(subscription);
        drop(event_client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn run_finished_records_bumped_unchanged_produced_port_output() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![cell(
            "producer",
            "result = cached",
            1,
            dag(vec![port("result")], vec![], None),
        )]));
        PortStore::open_at(temp.path())
            .expect("open ports")
            .put("result", &ipc_bytes())
            .expect("seed result");
        let runner = FakeRunner::default();
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let event_client = sequencer.client();
        let mut subscription = event_client.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        )
        .with_event_draft_sender(event_client.draft_sender());

        engine.rebuild_graph().expect("graph");
        engine.begin_source_cascade().await;
        let before = engine
            .produced_port_versions("producer")
            .expect("before versions");
        engine
            .run_cell_with_status_events("producer")
            .await
            .expect("run producer");
        engine
            .bump_produced_ports_if_unchanged("producer", &before)
            .expect("bump unchanged");
        let outputs = engine
            .produced_port_output_refs("producer", &before)
            .expect("output refs");
        engine
            .emit_run_report("producer", CellRunStatus::Succeeded, outputs)
            .await
            .expect("finish run");

        let events = recv_port_event_kinds(&mut subscription, 3).await;
        assert!(matches!(
            &events[2],
            PortEventKind::RunFinished {
                cell_id,
                outputs,
                ..
            } if cell_id == "producer" && outputs == &vec![PortRef {
                port: "result".to_string(),
                version: 2,
                class: PortClass::Dataframe,
                schema_hash: None,
            }]
        ));

        drop(engine);
        drop(subscription);
        drop(event_client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn source_push_emits_cascade_error_on_failure() {
        let temp = TempDir::new().expect("temp dir");
        let store = store_with_notebook(notebook(vec![cell(
            "a",
            "a = spur.get('sales')",
            1,
            dag(vec![port("a")], vec![], Some(source("csv", "sales"))),
        )]));
        let runner = FakeRunner::default();
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let event_client = sequencer.client();
        let mut subscription = event_client.subscribe();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner,
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        )
        .with_event_draft_sender(event_client.draft_sender());

        let error = engine
            .process_source_push(SourcePush {
                source: source("csv", ""),
                payload: SourcePayload::IpcBytes(ipc_bytes()),
            })
            .await
            .expect_err("invalid source port should fail");
        assert!(error.to_string().contains("port name cannot be empty"));

        let events = recv_port_event_kinds(&mut subscription, 3).await;

        assert!(matches!(
            events.as_slice(),
            [
                PortEventKind::CascadeStarted { cascade_id: 1, .. },
                PortEventKind::CascadeError {
                    cascade_id: 1,
                    code,
                    port: Some(port),
                    ..
                },
                PortEventKind::CascadeFinished {
                    cascade_id: 1,
                    status: CascadeStatus::Failed,
                },
            ] if code == "port" && port.is_empty()
        ));
        drop(engine);
        drop(subscription);
        drop(event_client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn reactive_engine_client_exposes_handle_port_event_stream() {
        let (source_tx, _source_rx) = mpsc::channel(4);
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig::default());
        let handle = ReactiveEngineHandle {
            source_tx,
            shutdown_tx: None,
            task: tokio::spawn(async {}),
            event_sequencer: Some(sequencer),
        };
        let client = handle.client();
        let mut subscription = client
            .subscribe_port_events()
            .expect("spawned clients expose port events");

        handle
            .port_event_client()
            .emit(PortEventDraft::new(PortEventKind::CascadeError {
                cascade_id: 9,
                code: "port".to_string(),
                message: "source write failed".to_string(),
                port: Some("sales".to_string()),
            }))
            .await
            .expect("emit event");

        let event = subscription.recv().await.expect("port event");
        assert_eq!(event.seq(), 1);
        assert!(matches!(
            event.kind(),
            PortEventKind::CascadeError {
                cascade_id: 9,
                code,
                port: Some(port),
                ..
            } if code == "port" && port == "sales"
        ));

        drop(subscription);
        drop(client);
        handle.shutdown().await;
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
                payload: SourcePayload::IpcBytes(ipc_bytes()),
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
    async fn manual_ai_node_marked_stale_not_run_on_upstream_change() {
        let temp = TempDir::new().expect("temp dir");
        let mut root = notebook(vec![
            cell(
                "source",
                "source = spur.get('sales')",
                1,
                dag(vec![port("source")], vec![], Some(source("csv", "sales"))),
            ),
            cell(
                "ai",
                "Summarize sales",
                1,
                dag(vec![port("summary")], vec!["source"], None),
            ),
        ]);
        set_kernelspec(&mut root, "ai", "spur");
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let report = engine
            .process_source_push(SourcePush {
                source: source("csv", "sales"),
                payload: SourcePayload::IpcBytes(ipc_bytes()),
            })
            .await
            .expect("source push");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("source", CellRunStatus::Succeeded),
                CellRunReport::new("ai", CellRunStatus::Stale),
            ]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["source"]
        );
    }

    #[tokio::test]
    async fn live_ai_node_cascades_on_upstream_change() {
        let temp = TempDir::new().expect("temp dir");
        let mut root = notebook(vec![
            cell(
                "source",
                "source = spur.get('sales')",
                1,
                dag(vec![port("source")], vec![], Some(source("csv", "sales"))),
            ),
            cell(
                "ai",
                "Summarize sales",
                1,
                dag(vec![port("summary")], vec!["source"], None),
            ),
        ]);
        set_kernelspec(&mut root, "ai", "spur");
        set_ai_live(&mut root, "ai", true);
        let store = store_with_notebook(root);
        let runner = FakeRunner::default();
        let mut engine = ReactiveEngine::new(
            Arc::clone(&store),
            runner.clone(),
            temp.path().join("reactive.ipynb"),
            temp.path().to_path_buf(),
        );

        let report = engine
            .process_source_push(SourcePush {
                source: source("csv", "sales"),
                payload: SourcePayload::IpcBytes(ipc_bytes()),
            })
            .await
            .expect("source push");

        assert_eq!(
            report.runs,
            vec![
                CellRunReport::new("source", CellRunStatus::Succeeded),
                CellRunReport::new("ai", CellRunStatus::Succeeded),
            ]
        );
        assert_eq!(
            runner
                .requests()
                .iter()
                .map(|request| request.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec!["source", "ai"]
        );
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
        assert_eq!(ports.get("a").expect("a bumped").version(), 2);
        assert_eq!(ports.get("b").expect("b bumped").version(), 2);
        assert_eq!(ports.get("z").expect("z untouched").version(), 1);
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
            plugins: None,
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
    fn dag_status_snapshot_folds_ordered_port_events() {
        let root = notebook(vec![
            cell("a", "a = 1", 1, dag(vec![port("a")], vec![], None)),
            cell("b", "b = a", 1, dag(vec![], vec!["a"], None)),
        ]);
        let events = vec![
            PortEventKind::RunFinished {
                cascade_id: 1,
                run_id: 1,
                cell_id: "a".to_string(),
                status: RunStatus::Succeeded,
                outputs: vec![],
            },
            PortEventKind::RunStarted {
                cascade_id: 1,
                run_id: 2,
                cell_id: "a".to_string(),
                inputs: vec![],
            },
            PortEventKind::RunFinished {
                cascade_id: 1,
                run_id: 3,
                cell_id: "b".to_string(),
                status: RunStatus::UpstreamFailed,
                outputs: vec![],
            },
        ];

        let snapshot = build_dag_status_snapshot_from_events(
            &root,
            43,
            &events,
            BTreeMap::from([("a".to_string(), 8)]),
        );

        assert_eq!(snapshot["notebook_version"], 43);
        assert_eq!(snapshot["port_manifest"], json!({ "a": 8 }));
        assert_eq!(
            snapshot["nodes"],
            json!([
                { "id": "a", "state": "running", "execution_count": null },
                { "id": "b", "state": "upstream-failed", "execution_count": null },
            ])
        );
    }

    #[test]
    fn debounce_keeps_latest_push_per_source_and_honors_in_flight_cap() {
        let mut debounce = SourceDebounce::new(ReactiveEngineConfig {
            source_debounce: std::time::Duration::from_millis(25),
            max_in_flight: 2,
        });

        debounce.push(SourcePush {
            source: source("csv", "sales"),
            payload: SourcePayload::IpcBytes(vec![1]),
        });
        debounce.push(SourcePush {
            source: source("csv", "sales"),
            payload: SourcePayload::IpcBytes(vec![2]),
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
                    frontend: None,
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

    fn set_kernelspec(root: &mut NotebookRoot, id: &str, spec_name: &str) {
        for cell in &mut root.cells {
            if let Cell::Code(cell) = cell {
                if cell.id.as_deref() == Some(id) {
                    cell.metadata
                        .other
                        .insert("kernelspec".to_string(), json!(spec_name));
                    return;
                }
            }
        }
        panic!("missing cell {id}");
    }

    fn set_ai_live(root: &mut NotebookRoot, id: &str, live: bool) {
        for cell in &mut root.cells {
            if let Cell::Code(cell) = cell {
                if cell.id.as_deref() == Some(id) {
                    cell.metadata
                        .other
                        .insert("ai_live".to_string(), json!(live));
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
            class: None,
            schema: None,
        }
    }

    fn port_with_schema(port: &str, schema: Value) -> PortSpec {
        PortSpec {
            port: port.to_string(),
            repr: "arrow".to_string(),
            display: None,
            class: Some("dataframe".to_string()),
            schema: Some(schema),
        }
    }

    fn source(kind: &str, port: &str) -> DagSource {
        DagSource {
            kind: kind.to_string(),
            port: port.to_string(),
            class: None,
            schema: None,
        }
    }

    fn source_with_schema(kind: &str, port: &str, schema: Value) -> DagSource {
        DagSource {
            kind: kind.to_string(),
            port: port.to_string(),
            class: Some("dataframe".to_string()),
            schema: Some(schema),
        }
    }

    fn declared_schema_json(data_type: &str) -> Value {
        json!({
            "fields": [
                {
                    "name": "value",
                    "data_type": data_type,
                    "nullable": false,
                    "dict_id": 0,
                    "dict_is_ordered": false,
                    "metadata": {}
                }
            ],
            "metadata": {}
        })
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
