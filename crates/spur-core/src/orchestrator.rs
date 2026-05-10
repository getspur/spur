use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;
use tracing::{debug, error, info, warn};

use spur_acp::config::{SpurConfig, WorktreeConfig};
use spur_acp::connection::AgentConnection;
use spur_acp::registry::AgentRegistry;
use spur_acp::session_lock::{AcquireOutcome, SessionAttachGuard};
use spur_acp::types::*;
use spur_acp::{
    CancellationControl, DelegationAbortHandle, DelegationAbortReason, DelegationDispatchError,
    DelegationResult, DelegationStatus, LifecycleState, ReviewKind, ReviewPayload, SpurEvent,
    SpurEventBody, TimeoutFallback,
};
use spur_pm::Issue;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, McpServer, McpServerHttp, PromptRequest, ProtocolVersion,
    SessionUpdate, SetSessionModeRequest, TextContent,
};

use spur_blob_store::{
    ContentType, MeasuredOutcomeStore, OutcomeKey, OutcomeMetadata, OutcomeStore,
};
use spur_cost::CostTracker;
use spur_license::SpurLicense;
use spur_mcp::tools::BaseSpec;
use spur_mcp::worker_server::WorkerMcpServer;
use spur_mcp::{
    build_worker_info, DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo,
};

use dashmap::DashMap;
use spur_pm::PmService;
use spur_worktree::git_blob_store::GitBlobOutcomeStore;
use spur_worktree::{manager::WorktreeError, WorktreeManager};

use crate::lineage::ExecutorId;
use crate::review_sink::ReviewSink;
use crate::scheduler::TurnGuard;

pub mod adhoc;
pub mod connection;
mod delegation;
pub mod input;
pub mod interactive_loop;
mod plan_ops;
mod pm_bridge;
pub mod prompt;
mod review;
pub mod session;
mod session_discovery;
mod support;
pub mod types;
mod util;
mod worker_mcp;

pub use delegation::cleanup::{should_commit_worker_diff, should_preserve_worktree};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use delegation::execute::{render_retry_context, RetryAttempt};
use input::strip_bang_prefix;
pub use input::InteractiveInput;
use plan_ops::load_plan_summaries;
use pm_bridge::{handle_get_issue_graph, issue_to_detail_event, refresh_pm_state};
#[cfg(any(test, feature = "test-support"))]
use review::apply_decision_to_candidate;
pub use review::{cleanup_cancelled_review, review_dispatcher_loop};
use session::{abort_mcp_handle, cleanup_mcp_on_err, retire_brain_session};
use session_discovery::classify_sessions;
pub use types::{
    ActiveConnection, BrainSession, FaultInjectionHooks, LoadBrainSessionError, ReconnectError,
    RunOpts, RunResult,
};
pub use util::normalize_agent_name;
use util::{
    arm_cancel_deadline, binary_on_path, cancel_mode_for, format_error_chain, is_connection_death,
    reconnect_failure_event, render_beads_startup_warning, shellexpand_tilde,
    startup_beads_warning,
};
use worker_mcp::{build_worker_mcp_servers_with, WorkerMcpFetcher};

type McpGuarded<T> = (T, AbortOnDropHandle<()>);
type BrainRunBootstrap = (
    Box<dyn spur_acp::AgentConnection>,
    JoinHandle<()>,
    bool,
    Option<String>,
    SessionId,
);
type NewBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    agent_client_protocol::schema::NewSessionResponse,
    spur_acp::BrainSessionId,
    SessionId,
);
type LoadedBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    String,
    Option<std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>>,
    bool,
    spur_acp::LoadOutcome,
    spur_acp::BrainSessionId,
    SessionId,
);

const MAX_SESSION_LIST_PAGES: usize = 1000;
/// Cap session listings at a number appropriate for local CLI agents.
/// 100k was excessive; even power users rarely exceed a few hundred
/// sessions per agent.
const MAX_SESSION_LIST_SESSIONS: usize = 1_000;

// ─── Orchestrator ────────────────────────────────────────────────────

/// The central orchestrator that drives the brain-worker pipeline.
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktree_authority: Arc<crate::WorktreeAuthority>,
    pub self_held: spur_acp::session_liveness::SelfHeldSet,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
    /// Monotonic sequence counter for the S2 funnel. The funnel task
    /// owns the write end via `fetch_add`; retained on the struct so
    /// tests/diagnostics can inspect the current count if needed.
    #[allow(dead_code)]
    event_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// S2 funnel handle — every orchestrator emit flows through this.
    /// Internally writes `SpurEventBody` into an mpsc that the funnel
    /// task drains onto `event_tx`, stamping monotonic `seq` +
    /// `occurred_at` in strict enqueue order (Pitfall P1).
    funnel: crate::event_funnel::FunnelHandle,
    pub review_sink: ReviewSink, // Clone type, shares inner Arc<Mutex>
    repo_root: PathBuf,
    pub pm_service: Option<Arc<PmService>>,
    outcome_store: Arc<dyn OutcomeStore>,
    /// Background tokio tasks owned by the orchestrator.
    background_tasks: Vec<JoinHandle<()>>,
    /// INV-6: per-delegation cancellation token registry.
    cancellation_control: CancellationControl,
    /// Sender half of the `run_interactive` ingress channel.  Set by
    /// `set_continuation_tx` so the MCP server can route detached
    /// delegation completions back to the orchestrator.
    continuation_tx: Option<mpsc::Sender<InteractiveInput>>,
    /// Overflow buffer for detached continuations.  Mirrors the buffer
    /// passed to `run_interactive`; set alongside `continuation_tx`.
    continuation_overflow: Option<crate::continuation_bridge::OverflowBuf>,
    /// Feature gate for dynamic quota/feature enforcement.
    feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
    pub(crate) peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
    /// Per-`BrainSession` worker MCP servers, lazily started on first
    /// dispatch with `enable_worker_mcp = true`. Phase 5 / Task 25 —
    /// the field exists; population happens via
    /// [`Orchestrator::ensure_worker_mcp_server`]. Wiring into the
    /// dispatch path lands in a follow-up task.
    pub(crate) worker_mcp_servers: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    fault_injection_hooks: FaultInjectionHooks,
    /// Abort handle for the production peer-mailbox reconciler task spawned
    /// by `Orchestrator::new` when `peer_mailbox_enabled = true`. Stored
    /// directly so introspection does not depend on `background_tasks`
    /// insertion order. The task itself is still tracked in
    /// `background_tasks` for `Drop` to abort.
    pub(crate) peer_mailbox_reconciler_abort: Option<tokio::task::AbortHandle>,
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        for handle in self.background_tasks.drain(..) {
            handle.abort();
        }
    }
}

impl Orchestrator {
    /// Create a new orchestrator for the given repo directory.
    pub fn new(
        repo_root: PathBuf,
        config: SpurConfig,
        feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
    ) -> Result<Self> {
        let registry = AgentRegistry::load(config.agents.entries.clone());
        let outcome_store: Arc<dyn OutcomeStore> = Arc::new(MeasuredOutcomeStore::new(
            GitBlobOutcomeStore::new(repo_root.clone()),
        ));
        let self_held = spur_acp::session_liveness::SelfHeldSet::new();

        // Try to open cost tracker (non-fatal if it fails).
        let cost_tracker = {
            let db_path = shellexpand_tilde(&config.cost.db_path);
            if let Some(parent) = Path::new(&db_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match CostTracker::open(Path::new(&db_path)) {
                Ok(ct) => Some(ct),
                Err(e) => {
                    warn!(error = %e, "Failed to open cost database, cost tracking disabled");
                    None
                }
            }
        };

        // S1.d — 4096 supports ~2.5s of events at 1600 evt/s peak
        // (20 workers × 80 evt/s). Subscribers that still lag get
        // RecvError::Lagged (logged at WARN; see S1.d Lagged audit).
        let (event_tx, _) = broadcast::channel(4096);
        // S2 — spawn the singleton funnel. Every orchestrator emit
        // flows through `funnel.emit(body)`; the funnel task stamps
        // monotonic seq + wall-clock time and forwards on `event_tx`.
        let event_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let lineage =
            std::sync::Arc::new(std::sync::Mutex::new(crate::lineage::ExecutorLineage::new()));
        let funnel = crate::event_funnel::spawn_funnel_with_lineage(
            event_tx.clone(),
            event_seq.clone(),
            lineage,
        );
        let worktree_authority = Arc::new(crate::WorktreeAuthority::new(
            repo_root.clone(),
            self_held.clone(),
            funnel.clone(),
            crate::AuthorityConfig::default(),
        ));
        // S3 — durable JSONL sink subscribes to the same broadcast.
        let max_bytes = feature_gate
            .as_ref()
            .and_then(|g| g.quota(spur_license::QuotaKey::EventRetentionBytes))
            .and_then(|v| v.as_bytes())
            .unwrap_or(crate::event_sink::DEFAULT_MAX_BYTES);
        let max_total_bytes = config.log.events_max_total_bytes;
        crate::event_sink::spawn_sink(event_tx.subscribe(), max_bytes, max_total_bytes);
        let review_sink = ReviewSink::new();

        let mut orchestrator = Self {
            registry,
            config,
            worktree_authority: worktree_authority.clone(),
            self_held,
            cost_tracker,
            event_tx,
            event_seq,
            funnel,
            review_sink,
            repo_root,
            pm_service: None,
            outcome_store,
            background_tasks: Vec::new(),
            cancellation_control: CancellationControl::new(),
            continuation_tx: None,
            continuation_overflow: None,
            feature_gate,
            peer_mailbox: None,
            worker_mcp_servers: Arc::new(DashMap::new()),
            fault_injection_hooks: FaultInjectionHooks::default(),
            peer_mailbox_reconciler_abort: None,
        };

        let ttl_days: u64 = match std::env::var("SPUR_OUTCOME_TTL_DAYS") {
            Ok(raw) => match raw.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => {
                    tracing::warn!(
                        env = %raw,
                        "SPUR_OUTCOME_TTL_DAYS is set but not a positive integer; using default 7"
                    );
                    7
                }
            },
            Err(_) => 7,
        };
        let sweep_store = orchestrator.outcome_store.clone();
        let sweep_handle = tokio::spawn(async move {
            // saturating_mul caps at u64::MAX seconds for absurd inputs; the
            // sweep would take longer than the heat death of the universe but
            // wouldn't panic in debug or wrap silently in release.
            let ttl = Duration::from_secs(ttl_days.saturating_mul(86_400));
            match sweep_store.sweep_older_than(ttl).await {
                Ok(report) => tracing::info!(
                    target: "spur.metrics.outcome_swept",
                    namespaces_swept = report.namespaces_swept,
                    blobs_swept = report.blobs_swept,
                    bytes_freed = report.bytes_freed,
                    ttl_days,
                ),
                Err(e) => tracing::warn!(
                    target: "spur.metrics.outcome_swept_failed",
                    error = %e,
                ),
            }
        });
        orchestrator.background_tasks.push(sweep_handle);

        if orchestrator.config.peer_mailbox_enabled {
            let ledger: Arc<dyn crate::peer_mailbox::PeerMailboxLedger> =
                Arc::new(crate::peer_mailbox::InMemoryLedger::new());
            let (reconciler_tx, reconciler_rx) = tokio::sync::mpsc::unbounded_channel();
            let session_slot: Arc<tokio::sync::RwLock<Option<String>>> =
                Arc::new(Default::default());

            let router = Arc::new(crate::peer_mailbox::PeerMailboxRouter::new(
                ledger.clone(),
                orchestrator.funnel.clone(),
                reconciler_tx,
                crate::peer_mailbox::Limits::default(),
            ));
            let builder = Arc::new(
                crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder::new(ledger.clone()),
            );
            orchestrator.peer_mailbox = Some(crate::peer_mailbox::PeerMailboxBundle {
                router,
                builder,
                ledger: ledger.clone(),
                brain_session_id_slot: session_slot.clone(),
            });

            let reconciler_handle = tokio::spawn(crate::peer_mailbox::run_reconciler_loop(
                reconciler_rx,
                ledger,
                orchestrator.funnel.clone(),
                session_slot,
            ));
            orchestrator.peer_mailbox_reconciler_abort = Some(reconciler_handle.abort_handle());
            orchestrator.background_tasks.push(reconciler_handle);
        }

        // Startup sweep: spawn into background. self_held is empty at boot;
        // the periodic sweeps + Live-probe semantics carry the safety
        // guarantee. See spec §6 risk table.
        let startup_auth = worktree_authority.clone();
        let startup_handle = tokio::spawn(async move {
            match startup_auth.sweep_once().await {
                Ok(report) => tracing::info!(
                    target: "spur.metrics.worktree_authority.startup",
                    probed = report.probed,
                    swept = report.swept,
                    skipped_unknown_owner = report.skipped_unknown_owner,
                    skipped_live = report.skipped_live,
                    salvage_committed = report.salvage_committed,
                    salvage_commit_failed = report.salvage_commit_failed,
                    salvage_ref_moved = report.salvage_ref_moved,
                    salvage_ref_failed = report.salvage_ref_failed,
                    "startup worktree authority sweep complete"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    "startup worktree authority sweep failed"
                ),
            }
        });
        orchestrator.background_tasks.push(startup_handle);

        // Periodic sweep — Drop impl on Orchestrator aborts every JoinHandle
        // in background_tasks (orchestrator.rs:918-923).
        let periodic = worktree_authority.spawn_periodic();
        orchestrator.background_tasks.push(periodic);

        Ok(orchestrator)
    }

    /// Attach a PM service. Must be called before `run_adhoc` or `run_interactive`.
    pub fn with_pm_service(mut self, pm: Arc<PmService>) -> Self {
        self.pm_service = Some(pm);
        self
    }

    /// Clone the orchestrator's event funnel so adjacent frontend tasks can
    /// emit through the same sequencing path as the orchestrator.
    pub fn event_funnel_handle(&self) -> crate::event_funnel::FunnelHandle {
        self.funnel.clone()
    }

    pub fn with_fault_injection_hooks(mut self, hooks: FaultInjectionHooks) -> Self {
        self.fault_injection_hooks = hooks;
        self
    }

    /// Execute a task directly on a single agent (no brain, no delegation).
    pub async fn exec_direct(&mut self, agent_name: &str, task: &str) -> Result<RunResult> {
        let start = Instant::now();
        let session_id = SessionId::new();

        let agent_config = self
            .registry
            .get(agent_name)
            .ok_or_else(|| anyhow!("Agent '{}' not found in registry", agent_name))?
            .clone();

        info!(agent = %agent_name, session = %session_id, "Direct execution");

        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                agent_name,
                "worker",
                None,
                task,
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let mut connection = self.create_connection(&agent_config, None);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize agent")?;

        let session_response = crate::skip_perm::new_session_with_bypass(
            &mut *connection,
            &agent_config,
            self.repo_root.clone(),
            vec![],
        )
        .await
        .context("Failed to create agent session")?;

        let prompt_request = PromptRequest::new(
            session_response.session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(task.to_string()))],
        );

        let success = true;
        crate::notification_drain::drive_prompt_notifications(
            &mut *connection,
            prompt_request,
            |notification| match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        print!("{}", tc.text);
                    }
                }
                _ => {}
            },
        )
        .await?;

        let _ = connection.shutdown().await;
        let duration = start.elapsed();

        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, agent_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(agent_config.cost_tier, duration);
        println!();

        Ok(RunResult {
            session_id,
            success,
            pr_url: None,
            total_cost_usd: total_cost,
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
