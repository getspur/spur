//! Typed command orchestration for deterministic code-evaluation runs.

use crate::metrics::{
    aggregate_metrics, score_retrieval, CaseKey, CaseMetricInput, Denominators, MetricError,
    MetricSuite, OperationalFlags, OperationalInput, PublishedMetrics, RankedEvidence,
    RetrievalInput, RetrievalMetrics, SuiteCaseInput,
};
use crate::model::{ModelCaseStatus, ModelPendingReason};
use crate::report::{
    BenchmarkReport, DeterministicReport, ReleaseInputs, ReportError, ReproducibilityMetadata,
    SuiteReport,
};
use crate::{
    content_sha256, retrieve, ArtifactError, ArtifactKind, ArtifactRecord, ArtifactStore,
    BackendCall, BackendResponse, ContentPin, ContractError, LeakagePolicy, QueryBackend,
    QueryBackendFuture, QueryError, RepositoryPin, RetrievalRequest, RunManifest,
    RunPhase as ArtifactPhase,
};
use clap::{Parser, Subcommand};
use futures::executor::block_on;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};
use thiserror::Error;

const CASE_ID: &str = "fixture";
const FIXTURE_SOURCE: &[u8] = b"pub fn lookup(value: u64) -> u64 {\n    value + 1\n}\n";
const REPORT_FILE: &str = "report.json";
const REPORT_CHECKSUM_FILE: &str = "report.sha256";

/// Command-line surface for one reproducible benchmark run.
#[derive(Debug, Parser)]
#[command(
    name = "spur-code-eval",
    about = "Reproducible code-intelligence benchmark runner"
)]
pub struct Cli {
    /// Run directory containing immutable artifacts and reports.
    #[arg(long, value_name = "PATH")]
    pub run_dir: PathBuf,
    /// Use the injected local fixture backend without network or credentials.
    #[arg(long)]
    pub fixture: bool,
    /// Phase command to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Stable benchmark command names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Validate fixture pins, policy, and denominators.
    Validate,
    /// Materialize and index the isolated fixture source.
    Index,
    /// Execute leakage-safe retrieval through the fixture backend.
    Retrieve,
    /// Freeze deterministic inputs and compute published metrics.
    Score,
    /// Attempt the optional advisory model lane.
    Model,
    /// Resume from the first incomplete verified phase.
    Resume,
    /// Render a checksum-verified deterministic or full report.
    Report,
}

impl Command {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Validate => "validate",
            Self::Index => "index",
            Self::Retrieve => "retrieve",
            Self::Score => "score",
            Self::Model => "model",
            Self::Resume => "resume",
            Self::Report => "report",
        }
    }
}

/// Fine-grained runner states derived from verified immutable artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// No validated run exists yet.
    Ready,
    /// Source pins and denominator validation are recorded.
    Validated,
    /// The isolated fixture index is recorded.
    Indexed,
    /// Rankings and contexts are recorded and ready to freeze.
    Retrieved,
    /// Deterministic artifacts and metrics are frozen.
    DeterministicScored,
    /// Advisory model work is pending without affecting deterministic status.
    ModelPending,
    /// Advisory model work completed and is frozen.
    ModelScored,
    /// A checksum-verified report is published.
    Reported,
}

/// Typed, contextual runner failure.
#[derive(Debug, Error)]
pub enum RunnerError {
    /// A command attempted to skip its required predecessor.
    #[error(
        "phase={command} case={case}: current state {current:?} requires completed {required}"
    )]
    PhaseOrder {
        /// Command that was rejected.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Verified current state.
        current: RunPhase,
        /// Required predecessor phase.
        required: &'static str,
    },
    /// Fixture-free execution is intentionally unavailable in this task.
    #[error("phase=startup case={CASE_ID}: --fixture is required; live backends are disabled")]
    FixtureRequired,
    /// Existing runner state is incomplete or contradictory.
    #[error("phase={command} case={case}: invalid runner state: {message}")]
    InvalidState {
        /// Command observing the invalid state.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Contextual explanation.
        message: String,
    },
    /// Immutable artifact lifecycle operation failed.
    #[error("phase={command} case={case}: artifact lifecycle: {source}")]
    Artifact {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Existing typed lifecycle error.
        #[source]
        source: ArtifactError,
    },
    /// Canonical contract construction failed.
    #[error("phase={command} case={case}: contract: {source}")]
    Contract {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Existing typed contract error.
        #[source]
        source: ContractError,
    },
    /// Leakage-safe query execution failed.
    #[error("phase={command} case={case}: query: {source}")]
    Query {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Existing typed query error.
        #[source]
        source: QueryError,
    },
    /// Deterministic scoring failed.
    #[error("phase={command} case={case}: scoring: {source}")]
    Metric {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Existing typed metric error.
        #[source]
        source: MetricError,
    },
    /// Checksum-safe report construction failed.
    #[error("phase={command} case={case}: report: {source}")]
    Report {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Existing typed report error.
        #[source]
        source: ReportError,
    },
    /// JSON encoding or metadata decoding failed.
    #[error("phase={command} case={case}: JSON: {source}")]
    Json {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// Local filesystem work failed.
    #[error("phase={command} case={case}: {operation} {path}: {source}")]
    Io {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Attempted operation.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// Read-only process metadata could not be collected exactly.
    #[error("phase={command} case={case}: reproducibility metadata: {message}")]
    Metadata {
        /// Command that failed.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Contextual explanation.
        message: String,
    },
    /// A persisted report does not match its checksum.
    #[error(
        "phase={command} case={case}: report checksum mismatch: expected {expected}, actual {actual}"
    )]
    ReportChecksum {
        /// Command that detected the mismatch.
        command: &'static str,
        /// Stable case identity.
        case: &'static str,
        /// Persisted expected digest.
        expected: String,
        /// Actual report digest.
        actual: String,
    },
}

/// Reproducible benchmark runner using one injected, network-free backend.
pub struct Runner {
    run_dir: PathBuf,
    argv: Vec<String>,
    backend: Box<dyn RunnerBackend>,
}

impl Runner {
    /// Creates the runner selected by parsed CLI arguments.
    ///
    /// # Errors
    ///
    /// Returns [`RunnerError::FixtureRequired`] when fixture mode is absent;
    /// live network and credential-backed execution is outside this runner.
    pub fn from_cli(cli: &Cli) -> Result<Self, RunnerError> {
        if !cli.fixture {
            return Err(RunnerError::FixtureRequired);
        }
        Ok(Self {
            run_dir: cli.run_dir.clone(),
            argv: std::env::args().collect(),
            backend: Box::new(FixtureBackend),
        })
    }

    /// Executes one typed command without permitting a phase regression.
    ///
    /// # Errors
    ///
    /// Returns a contextual [`RunnerError`] for invalid ordering, tampering,
    /// malformed metadata, scoring, query, report, or filesystem failures.
    pub fn run(&self, command: Command) -> Result<(), RunnerError> {
        if command == Command::Resume {
            self.resume()
        } else {
            self.execute(command)
        }
    }

    fn execute(&self, command: Command) -> Result<(), RunnerError> {
        match command {
            Command::Validate => self.validate(),
            Command::Index => self.index(),
            Command::Retrieve => self.retrieve(),
            Command::Score => self.score(),
            Command::Model => self.model(),
            Command::Report => self.report(),
            Command::Resume => self.resume(),
        }
    }

    fn validate(&self) -> Result<(), RunnerError> {
        let command = Command::Validate;
        let current = self.current_phase(command)?;
        transition(current, command)?;
        let started = Instant::now();
        let mut store = self.open_or_create(command)?;
        self.backend
            .materialize(store.root())
            .map_err(|source| io_error(command, "materialize fixture", store.root(), source))?;
        let reproducibility = self.reproducibility(command, started.elapsed(), &store, false)?;
        let payload = json!({
            "fixture": true,
            "source_sha256": content_sha256(FIXTURE_SOURCE),
            "denominators": suite_denominators(),
        });
        let bytes = phase_bytes(RunPhase::Validated, reproducibility, payload, command)?;
        store
            .write_atomic(ArtifactKind::Validation, &bytes)
            .map_err(|source| artifact_error(command, source))?;
        println!("phase=validate status=completed");
        Ok(())
    }

    fn index(&self) -> Result<(), RunnerError> {
        let command = Command::Index;
        let current = self.current_phase(command)?;
        transition(current, command)?;
        let started = Instant::now();
        let mut store = self.open_store(command)?;
        let reproducibility = self.reproducibility(command, started.elapsed(), &store, false)?;
        let payload = self.backend.index_payload();
        let bytes = phase_bytes(RunPhase::Indexed, reproducibility, payload, command)?;
        store
            .write_atomic(ArtifactKind::CallGraphs, &bytes)
            .map_err(|source| artifact_error(command, source))?;
        println!("phase=index status=completed");
        Ok(())
    }

    fn retrieve(&self) -> Result<(), RunnerError> {
        let command = Command::Retrieve;
        let current = self.current_phase(command)?;
        transition(current, command)?;
        let started = Instant::now();
        let mut store = self.open_store(command)?;
        let request = RetrievalRequest::new(
            store.root(),
            "find fixture lookup behavior",
            "lookup",
            1,
            1,
            LeakagePolicy::new(Vec::new(), Vec::new(), Vec::new())
                .map_err(|source| query_error(command, source))?,
        )
        .map_err(|source| query_error(command, source))?;
        let result = block_on(retrieve(self.backend.as_ref(), &request))
            .map_err(|source| query_error(command, source))?;
        let reproducibility = self.reproducibility(command, started.elapsed(), &store, false)?;
        let rankings = phase_bytes(
            RunPhase::Retrieved,
            reproducibility.clone(),
            json!({"result": result}),
            command,
        )?;
        let contexts = phase_bytes(
            RunPhase::Retrieved,
            reproducibility,
            json!({"context": "fixture lookup increments its input"}),
            command,
        )?;
        store
            .write_atomic(ArtifactKind::Rankings, &rankings)
            .map_err(|source| artifact_error(command, source))?;
        store
            .write_atomic(ArtifactKind::Contexts, &contexts)
            .map_err(|source| artifact_error(command, source))?;
        println!("phase=retrieve status=completed");
        Ok(())
    }

    fn score(&self) -> Result<(), RunnerError> {
        let command = Command::Score;
        let current = self.current_phase(command)?;
        transition(current, command)?;
        let started = Instant::now();
        let mut store = self.open_store(command)?;
        if store.manifest().phase() == ArtifactPhase::Prepared {
            store
                .freeze()
                .map_err(|source| artifact_error(command, source))?;
        }
        if store.manifest().artifact(ArtifactKind::Metrics).is_none() {
            let published = fixture_metrics(command)?.0;
            let reproducibility =
                self.reproducibility(command, started.elapsed(), &store, false)?;
            let bytes = phase_bytes(
                RunPhase::DeterministicScored,
                reproducibility,
                json!({"published_metrics": published}),
                command,
            )?;
            store
                .write_atomic(ArtifactKind::Metrics, &bytes)
                .map_err(|source| artifact_error(command, source))?;
        }
        store
            .transition(ArtifactPhase::DeterministicScored)
            .map_err(|source| artifact_error(command, source))?;
        println!("phase=score status=completed");
        Ok(())
    }

    fn model(&self) -> Result<(), RunnerError> {
        let command = Command::Model;
        let current = self.current_phase(command)?;
        let next = transition(current, command)?;
        if next == current {
            println!("phase=model status=pending reused=true");
            return Ok(());
        }
        let started = Instant::now();
        let mut store = self.open_store(command)?;
        let reproducibility = self.reproducibility(command, started.elapsed(), &store, false)?;
        let bytes = phase_bytes(
            RunPhase::ModelPending,
            reproducibility,
            json!({
                "status": ModelCaseStatus::ModelPending(
                    ModelPendingReason::MissingCredentials
                )
            }),
            command,
        )?;
        store
            .write_atomic(ArtifactKind::ModelRecords, &bytes)
            .map_err(|source| artifact_error(command, source))?;
        println!("phase=model status=pending reason=missing_credentials");
        Ok(())
    }

    fn report(&self) -> Result<(), RunnerError> {
        let command = Command::Report;
        let current = self.current_phase(command)?;
        let next = transition(current, command)?;
        if next == current {
            self.verify_report(command)?;
            println!("phase=report status=completed reused=true");
            return Ok(());
        }
        let started = Instant::now();
        let store = self.open_store(command)?;
        let (published, retrieval) = fixture_metrics(command)?;
        let repoqa = SuiteReport::new(repoqa_denominators(), Some(retrieval))
            .map_err(|source| report_error(command, source))?;
        let empty = Denominators::default();
        let crosscodeeval =
            SuiteReport::new(empty, None).map_err(|source| report_error(command, source))?;
        let jcg = SuiteReport::new(empty, None).map_err(|source| report_error(command, source))?;
        let deterministic = DeterministicReport::new(repoqa, crosscodeeval, jcg, published);
        let reproducibility = self.reproducibility(command, started.elapsed(), &store, true)?;
        let model_complete = store.manifest().phase() == ArtifactPhase::ModelScored;
        let report = BenchmarkReport::new(
            ReleaseInputs::new(true, true, true, model_complete, model_complete),
            reproducibility.clone(),
            deterministic,
            None,
        )
        .map_err(|source| report_error(command, source))?;
        let payloads = verified_payloads(&store, &reproducibility.artifact_records, command)?;
        let bytes = report
            .render_json(&payloads)
            .map_err(|source| report_error(command, source))?;
        self.persist_report(command, &bytes)?;
        println!("phase=report status=completed release=publish_deterministic");
        Ok(())
    }

    fn resume(&self) -> Result<(), RunnerError> {
        let command = Command::Resume;
        let phase = self.current_phase(command)?;
        let first = match phase {
            RunPhase::Ready => Some(Command::Validate),
            RunPhase::Validated => Some(Command::Index),
            RunPhase::Indexed => Some(Command::Retrieve),
            RunPhase::Retrieved => Some(Command::Score),
            RunPhase::DeterministicScored => Some(Command::Model),
            RunPhase::ModelPending | RunPhase::ModelScored => Some(Command::Report),
            RunPhase::Reported => None,
        };
        println!("resumed_from={}", first.map_or("none", Command::as_str));
        let mut next = first;
        while let Some(phase_command) = next {
            self.execute(phase_command)?;
            next = match phase_command {
                Command::Validate => Some(Command::Index),
                Command::Index => Some(Command::Retrieve),
                Command::Retrieve => Some(Command::Score),
                Command::Score => Some(Command::Model),
                Command::Model => Some(Command::Report),
                Command::Report | Command::Resume => None,
            };
        }
        if phase == RunPhase::Reported {
            self.verify_report(command)?;
        }
        Ok(())
    }

    fn current_phase(&self, command: Command) -> Result<RunPhase, RunnerError> {
        let manifest_path = self.run_dir.join(ArtifactKind::Manifest.relative_path());
        if !manifest_path.exists() {
            if self.run_dir.exists()
                && fs::read_dir(&self.run_dir)
                    .map_err(|source| {
                        io_error(command, "read run directory", &self.run_dir, source)
                    })?
                    .next()
                    .is_some()
            {
                return Err(invalid_state(
                    command,
                    "non-empty run directory has no manifest",
                ));
            }
            return Ok(RunPhase::Ready);
        }
        let store = self.open_store(command)?;
        let phase = phase_from_store(&store, command)?;
        if self.run_dir.join(REPORT_CHECKSUM_FILE).exists()
            && !self.run_dir.join(REPORT_FILE).exists()
        {
            return Err(invalid_state(
                command,
                "report checksum exists without report",
            ));
        }
        if self.run_dir.join(REPORT_FILE).exists()
            && self.run_dir.join(REPORT_CHECKSUM_FILE).exists()
        {
            if phase < RunPhase::DeterministicScored {
                return Err(invalid_state(
                    command,
                    "report exists before deterministic scoring",
                ));
            }
            self.verify_report(command)?;
            return Ok(RunPhase::Reported);
        }
        Ok(phase)
    }

    fn open_or_create(&self, command: Command) -> Result<ArtifactStore, RunnerError> {
        if self
            .run_dir
            .join(ArtifactKind::Manifest.relative_path())
            .exists()
        {
            return self.open_store(command);
        }
        let manifest =
            RunManifest::new(CASE_ID).map_err(|source| artifact_error(command, source))?;
        ArtifactStore::create(&self.run_dir, manifest)
            .map_err(|source| artifact_error(command, source))
    }

    fn open_store(&self, command: Command) -> Result<ArtifactStore, RunnerError> {
        ArtifactStore::open(&self.run_dir).map_err(|source| artifact_error(command, source))
    }

    fn reproducibility(
        &self,
        command: Command,
        elapsed: Duration,
        store: &ArtifactStore,
        frozen_only: bool,
    ) -> Result<ReproducibilityMetadata, RunnerError> {
        let (spur_revision, spur_dirty) = git_metadata(command)?;
        let mut phase_timings_micros = prior_timings(store, command)?;
        let micros = u64::try_from(elapsed.as_micros()).map_err(|error| RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: format!("phase timing overflow: {error}"),
        })?;
        phase_timings_micros.insert(command.as_str().to_owned(), micros);
        let (source_pins, repository_pins) = fixture_pins(command, &spur_revision)?;
        let artifact_records = store
            .manifest()
            .artifacts()
            .filter(|(_, record)| !frozen_only || record.is_frozen())
            .map(|(kind, record)| (kind, record.clone()))
            .collect();
        let index_bytes = store
            .manifest()
            .artifact(ArtifactKind::CallGraphs)
            .map_or(0, ArtifactRecord::bytes);
        Ok(ReproducibilityMetadata {
            spur_revision,
            spur_dirty,
            platform: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            command_argv: self.argv.clone(),
            phase_timings_micros,
            peak_rss_bytes: peak_rss_bytes(command)?,
            index_bytes,
            source_pins,
            repository_pins,
            query_policy_hash: content_sha256(b"fixture-query-policy-v1"),
            scorer_versions: BTreeMap::from([(
                "deterministic-metrics".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            )]),
            adapter_versions: BTreeMap::from([
                (
                    "crosscodeeval".to_owned(),
                    env!("CARGO_PKG_VERSION").to_owned(),
                ),
                ("jcg".to_owned(), env!("CARGO_PKG_VERSION").to_owned()),
                ("repoqa".to_owned(), env!("CARGO_PKG_VERSION").to_owned()),
            ]),
            suite_denominators: suite_denominators(),
            artifact_records,
        })
    }

    fn persist_report(&self, command: Command, bytes: &[u8]) -> Result<(), RunnerError> {
        let report_path = self.run_dir.join(REPORT_FILE);
        write_once(&report_path, bytes)
            .map_err(|source| io_error(command, "persist report", &report_path, source))?;
        let mut checksum = content_sha256(bytes).into_bytes();
        checksum.push(b'\n');
        let checksum_path = self.run_dir.join(REPORT_CHECKSUM_FILE);
        write_once(&checksum_path, &checksum)
            .map_err(|source| io_error(command, "persist report checksum", &checksum_path, source))
    }

    fn verify_report(&self, command: Command) -> Result<(), RunnerError> {
        let report_path = self.run_dir.join(REPORT_FILE);
        let checksum_path = self.run_dir.join(REPORT_CHECKSUM_FILE);
        let bytes = fs::read(&report_path)
            .map_err(|source| io_error(command, "read report", &report_path, source))?;
        let expected = fs::read_to_string(&checksum_path)
            .map_err(|source| io_error(command, "read report checksum", &checksum_path, source))?
            .trim()
            .to_owned();
        let actual = content_sha256(&bytes);
        if expected != actual {
            return Err(RunnerError::ReportChecksum {
                command: command.as_str(),
                case: CASE_ID,
                expected,
                actual,
            });
        }
        Ok(())
    }
}

fn transition(current: RunPhase, command: Command) -> Result<RunPhase, RunnerError> {
    let next = match (current, command) {
        (RunPhase::Ready, Command::Validate) => RunPhase::Validated,
        (RunPhase::Validated, Command::Index) => RunPhase::Indexed,
        (RunPhase::Indexed, Command::Retrieve) => RunPhase::Retrieved,
        (RunPhase::Retrieved, Command::Score) => RunPhase::DeterministicScored,
        (RunPhase::DeterministicScored | RunPhase::ModelPending, Command::Model) => {
            RunPhase::ModelPending
        }
        (
            RunPhase::DeterministicScored
            | RunPhase::ModelPending
            | RunPhase::ModelScored
            | RunPhase::Reported,
            Command::Report,
        ) => RunPhase::Reported,
        (_, Command::Resume) => current,
        _ => {
            let required = match command {
                Command::Validate => "ready",
                Command::Index => "validate",
                Command::Retrieve => "index",
                Command::Score => "retrieve",
                Command::Model | Command::Report => "score",
                Command::Resume => "verified state",
            };
            return Err(RunnerError::PhaseOrder {
                command: command.as_str(),
                case: CASE_ID,
                current,
                required,
            });
        }
    };
    Ok(next)
}

fn phase_from_store(store: &ArtifactStore, command: Command) -> Result<RunPhase, RunnerError> {
    let manifest = store.manifest();
    let has = |kind| manifest.artifact(kind).is_some();
    let deterministic_inputs = [
        ArtifactKind::Validation,
        ArtifactKind::CallGraphs,
        ArtifactKind::Rankings,
        ArtifactKind::Contexts,
    ];
    match manifest.phase() {
        ArtifactPhase::Prepared => {
            if !has(ArtifactKind::Validation) {
                if manifest.artifacts().next().is_some() {
                    return Err(invalid_state(
                        command,
                        "prepared run has artifacts before validation",
                    ));
                }
                Ok(RunPhase::Ready)
            } else if !has(ArtifactKind::CallGraphs) {
                Ok(RunPhase::Validated)
            } else if !has(ArtifactKind::Rankings) && !has(ArtifactKind::Contexts) {
                Ok(RunPhase::Indexed)
            } else if has(ArtifactKind::Rankings) && has(ArtifactKind::Contexts) {
                Ok(RunPhase::Retrieved)
            } else {
                Err(invalid_state(
                    command,
                    "rankings and contexts must be recorded together",
                ))
            }
        }
        ArtifactPhase::Frozen => {
            if deterministic_inputs.into_iter().all(has) {
                Ok(RunPhase::Retrieved)
            } else {
                Err(invalid_state(
                    command,
                    "frozen run is missing a deterministic input",
                ))
            }
        }
        ArtifactPhase::DeterministicScored => {
            if has(ArtifactKind::ModelRecords) {
                Ok(RunPhase::ModelPending)
            } else {
                Ok(RunPhase::DeterministicScored)
            }
        }
        ArtifactPhase::ModelScored => Ok(RunPhase::ModelScored),
    }
}

#[derive(Serialize)]
struct PhaseArtifact<T> {
    phase: RunPhase,
    case: &'static str,
    reproducibility: ReproducibilityMetadata,
    payload: T,
}

fn phase_bytes<T: Serialize>(
    phase: RunPhase,
    reproducibility: ReproducibilityMetadata,
    payload: T,
    command: Command,
) -> Result<Vec<u8>, RunnerError> {
    let mut bytes = serde_json::to_vec(&PhaseArtifact {
        phase,
        case: CASE_ID,
        reproducibility,
        payload,
    })
    .map_err(|source| json_error(command, source))?;
    bytes.push(b'\n');
    Ok(bytes)
}

trait RunnerBackend: QueryBackend {
    fn materialize(&self, root: &Path) -> io::Result<()>;
    fn index_payload(&self) -> Value;
}

struct FixtureBackend;

impl RunnerBackend for FixtureBackend {
    fn materialize(&self, root: &Path) -> io::Result<()> {
        write_once(&root.join("fixture.rs"), FIXTURE_SOURCE)
    }

    fn index_payload(&self) -> Value {
        json!({
            "schema": "fixture-call-graph-v1",
            "nodes": ["lookup"],
            "edges": [],
            "source_sha256": content_sha256(FIXTURE_SOURCE),
        })
    }
}

impl QueryBackend for FixtureBackend {
    fn dispatch<'a>(&'a self, _source_root: &'a Path, call: BackendCall) -> QueryBackendFuture<'a> {
        let body = match call.tool_name() {
            "knowledge_context_pack_2" => json!({
                "primary_evidence": [{
                    "file": "fixture.rs",
                    "stable_symbol_id": "graph://symbol/fixture-lookup",
                    "score": 1.0,
                    "line_range": [1, 3]
                }],
                "recommended_next_tools": [{
                    "tool": "code_read_symbol",
                    "selector": "graph://symbol/fixture-lookup"
                }],
                "staleness": {"analyst_matches_exact_graph": true}
            }),
            "code_symbol_search" => json!({
                "candidates": [{
                    "selector": "fixture.rs::lookup",
                    "uri": "graph://symbol/fixture-lookup",
                    "id": "fixture-lookup",
                    "file_path": "fixture.rs",
                    "line_range": [1, 3]
                }]
            }),
            "code_read_symbol" => json!({
                "symbol": {
                    "file_path": "fixture.rs",
                    "uri": "graph://symbol/fixture-lookup",
                    "id": "fixture-lookup",
                    "line_range": [1, 3]
                }
            }),
            _ => json!({"primary_evidence": []}),
        };
        Box::pin(async move { Ok(BackendResponse::new(body, Duration::from_micros(1))) })
    }
}

fn fixture_metrics(command: Command) -> Result<(PublishedMetrics, RetrievalMetrics), RunnerError> {
    let ranked = RankedEvidence::new(1.0, true).map_err(|source| metric_error(command, source))?;
    let retrieval_input =
        RetrievalInput::new(vec![ranked], 1).map_err(|source| metric_error(command, source))?;
    let retrieval =
        score_retrieval(&retrieval_input).map_err(|source| metric_error(command, source))?;
    let key = CaseKey::new(
        CASE_ID,
        MetricSuite::RepoQa,
        "fixture",
        "rust",
        "fixture-repository",
    )
    .map_err(|source| metric_error(command, source))?;
    let case = CaseMetricInput::eligible(
        key,
        SuiteCaseInput::RepoQa(retrieval_input),
        OperationalInput::new(1, 1, 1, OperationalFlags::from_signals(&[])),
    )
    .map_err(|source| metric_error(command, source))?;
    let published = aggregate_metrics(&[case]).map_err(|source| metric_error(command, source))?;
    Ok((published, retrieval))
}

fn repoqa_denominators() -> Denominators {
    Denominators {
        total: 1,
        eligible: 1,
        answered: 1,
        ..Denominators::default()
    }
}

fn suite_denominators() -> BTreeMap<MetricSuite, Denominators> {
    BTreeMap::from([
        (MetricSuite::RepoQa, repoqa_denominators()),
        (MetricSuite::CrossCodeEval, Denominators::default()),
        (MetricSuite::Jcg, Denominators::default()),
    ])
}

type FixturePins = (
    BTreeMap<String, ContentPin>,
    BTreeMap<String, RepositoryPin>,
);

fn fixture_pins(command: Command, revision: &str) -> Result<FixturePins, RunnerError> {
    let source_hash = content_sha256(FIXTURE_SOURCE);
    let source = ContentPin::new("fixture://code-eval/source", revision, &source_hash, "MIT")
        .map_err(|source| contract_error(command, source))?;
    let repository = RepositoryPin::new(
        "fixture://code-eval/repository",
        revision,
        None,
        source_hash,
    )
    .map_err(|source| contract_error(command, source))?;
    Ok((
        BTreeMap::from([("fixture-source".to_owned(), source)]),
        BTreeMap::from([("fixture-repository".to_owned(), repository)]),
    ))
}

fn prior_timings(
    store: &ArtifactStore,
    command: Command,
) -> Result<BTreeMap<String, u64>, RunnerError> {
    let mut timings = BTreeMap::new();
    for kind in [
        ArtifactKind::Validation,
        ArtifactKind::CallGraphs,
        ArtifactKind::Rankings,
        ArtifactKind::Metrics,
        ArtifactKind::ModelRecords,
    ] {
        if store.manifest().artifact(kind).is_none() {
            continue;
        }
        let path = store.artifact_path(kind);
        let bytes = fs::read(&path)
            .map_err(|source| io_error(command, "read phase metadata", &path, source))?;
        let line = bytes.split(|byte| *byte == b'\n').next().unwrap_or(&bytes);
        let value: Value =
            serde_json::from_slice(line).map_err(|source| json_error(command, source))?;
        if let Some(entries) = value
            .get("reproducibility")
            .and_then(|metadata| metadata.get("phase_timings_micros"))
            .and_then(Value::as_object)
        {
            for (phase, value) in entries {
                let micros = value.as_u64().ok_or_else(|| RunnerError::Metadata {
                    command: command.as_str(),
                    case: CASE_ID,
                    message: format!("phase timing {phase} is not a u64"),
                })?;
                timings.insert(phase.clone(), micros);
            }
        }
    }
    Ok(timings)
}

fn verified_payloads(
    store: &ArtifactStore,
    records: &BTreeMap<ArtifactKind, ArtifactRecord>,
    command: Command,
) -> Result<BTreeMap<ArtifactKind, Vec<u8>>, RunnerError> {
    records
        .keys()
        .map(|kind| {
            let mut file = store
                .open_verified(*kind)
                .map_err(|source| artifact_error(command, source))?;
            let mut bytes = Vec::new();
            io::copy(&mut file, &mut bytes).map_err(|source| {
                io_error(
                    command,
                    "read verified artifact",
                    store.artifact_path(*kind),
                    source,
                )
            })?;
            Ok((*kind, bytes))
        })
        .collect()
}

fn git_metadata(command: Command) -> Result<(String, bool), RunnerError> {
    let Some(root) = repository_root(command)? else {
        let snapshot = content_sha256(b"spur-code-eval-network-free-fixture-snapshot-v1");
        return Ok((snapshot.chars().take(40).collect(), true));
    };
    let root = root.to_str().ok_or_else(|| RunnerError::Metadata {
        command: command.as_str(),
        case: CASE_ID,
        message: "repository root is not UTF-8".to_owned(),
    })?;
    let revision = process_output(command, "git", &["-C", root, "rev-parse", "HEAD"])?;
    let revision = revision.trim().to_owned();
    if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: format!("git returned invalid revision {revision:?}"),
        });
    }
    let status = process_output(
        command,
        "git",
        &["-C", root, "status", "--porcelain", "--untracked-files=no"],
    )?;
    Ok((revision, !status.trim().is_empty()))
}

fn repository_root(command: Command) -> Result<Option<PathBuf>, RunnerError> {
    let current = std::env::current_dir()
        .map_err(|source| io_error(command, "read current directory", Path::new("."), source))?;
    for start in [current.as_path(), Path::new(env!("CARGO_MANIFEST_DIR"))] {
        if let Some(root) = start
            .ancestors()
            .find(|ancestor| ancestor.join(".git").exists())
        {
            return Ok(Some(root.to_path_buf()));
        }
    }
    Ok(None)
}

fn peak_rss_bytes(command: Command) -> Result<u64, RunnerError> {
    let pid = std::process::id().to_string();
    let output = process_output(command, "ps", &["-o", "rss=", "-p", &pid])?;
    let kib = output
        .split_whitespace()
        .next()
        .ok_or_else(|| RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: "ps returned no RSS value".to_owned(),
        })?
        .parse::<u64>()
        .map_err(|error| RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: format!("invalid RSS value: {error}"),
        })?;
    kib.checked_mul(1024).ok_or_else(|| RunnerError::Metadata {
        command: command.as_str(),
        case: CASE_ID,
        message: "RSS byte count overflow".to_owned(),
    })
}

fn process_output(
    command: Command,
    program: &str,
    arguments: &[&str],
) -> Result<String, RunnerError> {
    let output = ProcessCommand::new(program)
        .args(arguments)
        .output()
        .map_err(|error| RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: format!("cannot execute {program}: {error}"),
        })?;
    if !output.status.success() {
        return Err(RunnerError::Metadata {
            command: command.as_str(),
            case: CASE_ID,
            message: format!(
                "{program} exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    String::from_utf8(output.stdout).map_err(|error| RunnerError::Metadata {
        command: command.as_str(),
        case: CASE_ID,
        message: format!("{program} output is not UTF-8: {error}"),
    })
}

fn write_once(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.exists() {
        let existing = fs::read(path)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "immutable file already exists with different bytes",
        ));
    }
    let digest = content_sha256(bytes);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no UTF-8 file name")
        })?;
    let pending = path.with_file_name(format!(".{file_name}.{digest}.tmp"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&pending, path)?;
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
}

fn artifact_error(command: Command, source: ArtifactError) -> RunnerError {
    RunnerError::Artifact {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn contract_error(command: Command, source: ContractError) -> RunnerError {
    RunnerError::Contract {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn query_error(command: Command, source: QueryError) -> RunnerError {
    RunnerError::Query {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn metric_error(command: Command, source: MetricError) -> RunnerError {
    RunnerError::Metric {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn report_error(command: Command, source: ReportError) -> RunnerError {
    RunnerError::Report {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn json_error(command: Command, source: serde_json::Error) -> RunnerError {
    RunnerError::Json {
        command: command.as_str(),
        case: CASE_ID,
        source,
    }
}

fn io_error(
    command: Command,
    operation: &'static str,
    path: impl AsRef<Path>,
    source: io::Error,
) -> RunnerError {
    RunnerError::Io {
        command: command.as_str(),
        case: CASE_ID,
        operation,
        path: path.as_ref().to_path_buf(),
        source,
    }
}

fn invalid_state(command: Command, message: impl Into<String>) -> RunnerError {
    RunnerError::InvalidState {
        command: command.as_str(),
        case: CASE_ID,
        message: message.into(),
    }
}
