//! MCP callback server modules.
//!
//! - `types`: Shared data structures, constants, and RMCP helpers.
//! - `sync`: Label mutations and PM synchronization helpers.
//! - `plan_builder`: Epic subgraph topology and argument parsing.
//! - `recovery`: Startup recovery mechanics and quarantine.
//! - `review`: Clobber detection, diffing, and git worktree logic.
//! - `test_helpers`: Test injection hooks.
//! - `handlers`: RMCP Tool router and domain-specific handlers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use rmcp::{
    model::{
        object as rmcp_object, CallToolRequestParams, CallToolResult, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, OnceCell};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tracing::{debug, error, info};

use spur_acp::*;
use spur_license::FeatureKey;
use spur_pm::{IssueFilter, IssueSummary, IssueUpdate, PmService};
use spur_worktree::WorktreeManager;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::proposers::{CompositeProposer, RetryExhaustedProposer, TrivialScorer};
use crate::plan::reconciler::{
    Reconciler, ReconcilerConfig, ReconcilerDispatch, ReconcilerDispatchCtx,
};
use crate::plan::signal_watcher::SignalWatcher;
use spur_mcp::tools::{DelegationChannel, DelegationRequest};

pub(crate) mod handlers;
pub(crate) mod plan_builder;
pub(crate) mod plan_deps;
pub(crate) mod recovery;
pub(crate) mod review;
pub(crate) mod sync;
pub(crate) mod test_helpers;
pub(crate) mod types;

pub(crate) use crate::plan::continuation::{notify_fast_forward, ORPHAN_CLEAR_REASON_RESTART};
pub use crate::plan::continuation::{DetachedCompletionCallback, DetachedContinuationCtx};
pub(crate) use plan_builder::*;
pub use plan_builder::{
    build_entries_with_task_map, build_epic_subgraph, emit_plan_submit_audit,
    plan_epic_issue_creates, EpicSubgraph, PlanSubmitAuditContext,
};
pub(crate) use recovery::{replay_awaiting_review_continuation, AwaitingReviewReplay};
pub use spur_mcp::feature::community_feature_gate;
pub(crate) use spur_mcp::feature::{feature_error_message, require_feature};
#[cfg(any(test, feature = "test-support"))]
pub use spur_mcp::feature::{pro_feature_gate, unlicensed_feature_gate};
pub use spur_mcp::git::run_git_capture;
pub(crate) use sync::*;
pub use sync::{compensate_mutation_orphans, resolve_dispatch_orphan};
pub use types::*;

/// Brain/orchestrator MCP callback server.
///
/// Phase 4 field ownership (see
/// `docs/superpowers/plans/2026-06-21-phase4-plan-reconciler-core-extraction.md`):
/// the transport/session fields (`brain_session_id*`, `task_tracker`,
/// `feature_gate`, `retiring`, `cancel_token`, `root_handle`,
/// `root_shutdown_tx`, `tool_registry`, `inline_wait`, `event_sink`) are
/// infrastructure and stay in `spur-mcp`. The remaining fields — active plans,
/// plan registry, plan-ownership lock, reconciler handle/outcomes/config,
/// continuation context, cancellation control, outcome materialization, and the
/// delegation lifecycle handles — are orchestration-domain state that moves to
/// `spur-core`. The accessors below expose that subset for the staged move.
pub struct McpCallbackServer {
    /// Channel to send delegation requests to the orchestrator.
    pub(crate) delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Available worker agents (set once at creation).
    pub(crate) workers: Vec<WorkerInfo>,
    /// Brain session this server belongs to. INV-2: typed as BrainSessionId.
    pub(crate) brain_session_id: Arc<OnceCell<spur_acp::BrainSessionId>>,
    pub(crate) brain_session_id_notify: Arc<tokio::sync::Notify>,
    /// Delegation IDs whose background collector is still awaiting a result.
    pub(crate) active_delegations: Arc<tokio::sync::Mutex<HashSet<DelegationId>>>,
    /// Results that a background collector has received but the brain has
    /// not yet polled via `check_delegation_status`. Stored with insertion
    /// timestamp for TTL-based lazy eviction. Phase 4: normally empty —
    /// `BlockTimeout` collectors skip the write (INV-ASYNC-2) and the
    /// `AsyncRequested` path retired with `delegate_async` / `wait_delegation`.
    /// Retained as a TTL-bounded debug-injection buffer.
    pub(crate) completed_delegations:
        Arc<tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>>,
    /// Tracks spawned result-collector tasks for graceful shutdown.
    pub(crate) task_tracker: TaskTracker,
    /// Optional PM service for direct issue/PR operations.
    pub(crate) pm_service: Option<Arc<PmService>>,
    pub(crate) pm_service_like: Option<Arc<dyn crate::plan::PmLike>>,
    /// Optional event sink for emitting MCP lifecycle events.
    pub(crate) event_sink: Option<Arc<dyn spur_mcp::events::McpEventSink>>,
    /// Feature gate snapshot shared with the orchestrator/license runtime.
    pub(crate) feature_gate: Arc<spur_license::FeatureGate>,
    /// Active execution plans submitted via `submit_plan`.
    pub(crate) active_plans: Arc<tokio::sync::Mutex<HashMap<String, CachedPlan>>>,
    /// Ephemeral reconciler outcome buffers. MUST NOT be persisted to beads;
    /// durable plan state is reconstructed from beads on restart.
    pub(crate) reconciler_outcomes: Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>>,
    /// Phase 2.5 idempotency guard: maps `epic_id → plan_id` for the
    /// currently-active plan (if any). A sentinel `"__pending__"` value is
    /// used briefly during the PmService fetch to prevent concurrent
    /// `execute_epic` calls from racing into double-dispatch. Terminal plans
    /// are cleared lazily on the next `execute_epic` call for the same epic.
    pub(crate) plan_registry: Arc<tokio::sync::Mutex<crate::plan::PlanRegistry>>,
    /// Serializes current-brain ownership claims across `execute_epic` and
    /// `resume_plan`. The durable invariant lives in beads owner labels; this
    /// local lock closes scan-before-write races within one brain server.
    pub(crate) active_plan_claim_lock: Arc<tokio::sync::Mutex<()>>,
    /// INV-6: handle to the orchestrator's per-delegation cancellation token
    /// registry. `None` in test harnesses that don't wire a real orchestrator.
    pub(crate) cancellation_control: Option<CancellationControl>,
    /// Bundle of handles for routing detached delegation completions back
    /// into the orchestrator ingress via `report_detached_completion`.
    pub(crate) continuation_ctx: Arc<DetachedContinuationCtx>,
    pub(crate) materializer: OutcomeMaterializer,
    pub(crate) outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    /// Test-only hook used to force continuous epic audit churn between
    /// version reads while exercising retry bounds.
    pub(crate) version_churn_epic_for_test: Arc<tokio::sync::Mutex<Option<String>>>,
    /// Phase 1c: how long `handle_delegate_to_worker` / `handle_delegate_parallel`
    /// wait inline for a worker's oneshot to fire before handing the receiver
    /// to the detached collector. Default `0` — pure async-first.
    /// Wired from `SpurConfig.delegation.inline_wait_ms`.
    pub(crate) inline_wait: std::time::Duration,
    /// v3-c: set by `mark_retiring`; delegation entry points reject new
    /// requests once retirement begins.
    pub(crate) retiring: Arc<AtomicBool>,
    /// v3-c: parent cancellation token for in-flight collector tasks.
    pub(crate) cancel_token: CancellationToken,
    /// v3-c: handle to the root listener task so `force_abort` can stop it.
    pub(crate) root_handle: Mutex<Option<JoinHandle<()>>>,
    /// Graceful-shutdown signal for the root listener task.
    pub(crate) root_shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Handle to the optional beads reconciler task. It is enabled only after
    /// the orchestrator binds this server to a derived brain_session_id.
    pub(crate) reconciler_handle: Mutex<Option<ReconcilerTaskHandle>>,
    /// Optional startup recovery for legacy persisted plans. `start()` only
    /// decides whether it is needed; the task is spawned after the server has
    /// a brain_session_id so recovery can be owner-aware.
    pub(crate) startup_recovery: Mutex<StartupRecoveryState>,
    /// One-shot attach sweep that replays AwaitingReview continuations whose
    /// durable audit is present but whose live collector is no longer active.
    pub(crate) awaiting_review_rediscovery_started: AtomicBool,
    /// v0a.3: if true, `enable_reconciler` may spawn the reconciler after
    /// the brain_session_id is bound. Wired via `set_reconciler_enabled`.
    pub(crate) reconciler_enabled: bool,
    /// Fast-forward trigger for the reconciler. When the plan executor completes
    /// a task (transitions to AwaitingReview), it notifies the reconciler so it can
    /// immediately tick instead of waiting for the next interval. Only meaningful
    /// when `reconciler_enabled` is true.
    pub(crate) reconciler_fast_forward: Option<Arc<tokio::sync::Notify>>,
    /// Repository root for constructing paths used by beads-backed startup and
    /// plan automation. Set by `set_repo_root` before `start()`.
    pub(crate) repo_root: Option<std::path::PathBuf>,
    /// v0e: opt-in auto-merge/PR on durable epic completion.
    pub(crate) auto_merge_approved_plans: bool,
    /// Grace period before startup quarantines stale `spur:plan-pending`
    /// persisted-plan epics.
    pub(crate) plan_pending_grace: std::time::Duration,
    /// PR2 guard: when false, persisted plan reads always re-project from
    /// beads and never serve by the epic audit sequence token. PR3 advances
    /// the epic audit sequence on every task transition; once landed, this
    /// flag can be flipped on separately.
    pub(crate) versioned_cache_serve: bool,
    /// PR3 guard: when true, review_task uses the substrate-first
    /// non-advisory write path and invalidates the cache on exhausted retries.
    pub(crate) nonadvisory_review_writes: bool,
    /// Duration written into `spur:lease-expires-at:<ts>` labels for
    /// reconciler-owned persisted-plan dispatches.
    pub(crate) dispatch_lease_duration: std::time::Duration,
    /// Explicit code-graph MCP dependencies, including rebuild singleflight
    /// policy owned by `spur-graph`.
    pub(crate) graph_mcp_deps: spur_graph::mcp::GraphMcpDeps,
    /// Per-server brain tool registry. Core-owned orchestration modules are
    /// composed by the orchestrator at construction time.
    pub(crate) tool_registry: Arc<spur_mcp::registry::ToolRegistry>,
}

impl McpCallbackServer {
    /// Create a new MCP callback server.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(
        session_id: Option<&spur_acp::BrainSessionId>,
        pm_service: Option<Arc<PmService>>,
        event_sink: Option<Arc<dyn spur_mcp::events::McpEventSink>>,
        continuation_ctx: DetachedContinuationCtx,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> (Self, DelegationChannel) {
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);
        let materializer = OutcomeMaterializer::new(outcome_store.clone());

        let mut server = Self {
            delegation_tx: req_tx,
            workers: Vec::new(),
            brain_session_id: {
                let cell = Arc::new(OnceCell::new());
                if let Some(id) = session_id {
                    let _ = cell.set(id.clone());
                }
                cell
            },
            brain_session_id_notify: Arc::new(tokio::sync::Notify::new()),
            active_delegations: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            completed_delegations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            task_tracker: TaskTracker::new(),
            pm_service,
            pm_service_like: None,
            event_sink,
            feature_gate,
            active_plans: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            reconciler_outcomes: Arc::new(tokio::sync::Mutex::new(
                crate::plan::outcomes::OutcomeStore::default(),
            )),
            plan_registry: Arc::new(tokio::sync::Mutex::new(crate::plan::PlanRegistry::default())),
            active_plan_claim_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancellation_control: None,
            continuation_ctx: Arc::new(continuation_ctx),
            materializer,
            outcome_store,
            version_churn_epic_for_test: Arc::new(tokio::sync::Mutex::new(None)),
            inline_wait: std::time::Duration::from_millis(0),
            retiring: Arc::new(AtomicBool::new(false)),
            cancel_token: CancellationToken::new(),
            root_handle: Mutex::new(None),
            root_shutdown_tx: Mutex::new(None),
            reconciler_handle: Mutex::new(None),
            startup_recovery: Mutex::new(StartupRecoveryState::default()),
            awaiting_review_rediscovery_started: AtomicBool::new(false),
            reconciler_enabled: false,
            reconciler_fast_forward: None,
            repo_root: None,
            auto_merge_approved_plans: false,
            plan_pending_grace: DEFAULT_PLAN_PENDING_GRACE,
            versioned_cache_serve: false,
            nonadvisory_review_writes: false,
            dispatch_lease_duration: std::time::Duration::from_secs(600),
            graph_mcp_deps: spur_graph::mcp::GraphMcpDeps::default(),
            tool_registry: Arc::new(spur_mcp::registry::ToolRegistry::new()),
        };
        let tool_registry = crate::mcp::brain_tool_registry(
            crate::mcp::delegation::DelegationMcpDeps::from_server(&server),
            crate::mcp::plan::PlanMcpDeps::from_server(&server),
            crate::mcp::signals::SignalMcpDeps {
                pm_service: server.pm_service.clone(),
                event_sink: server.event_sink.clone(),
                feature_gate: Arc::clone(&server.feature_gate),
            },
        )
        .expect("core MCP tool registry must be valid");
        server.tool_registry = Arc::new(tool_registry);

        let channel = DelegationChannel { request_rx: req_rx };
        (server, channel)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_tool_registry(
        session_id: Option<&spur_acp::BrainSessionId>,
        pm_service: Option<Arc<PmService>>,
        event_sink: Option<Arc<dyn spur_mcp::events::McpEventSink>>,
        continuation_ctx: DetachedContinuationCtx,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
        feature_gate: Arc<spur_license::FeatureGate>,
        tool_registry: spur_mcp::registry::ToolRegistry,
    ) -> (Self, DelegationChannel) {
        let (mut server, channel) = Self::new(
            session_id,
            pm_service,
            event_sink,
            continuation_ctx,
            outcome_store,
            feature_gate,
        );
        server.set_tool_registry(tool_registry);
        (server, channel)
    }

    pub fn set_tool_registry(&mut self, tool_registry: spur_mcp::registry::ToolRegistry) {
        self.tool_registry = Arc::new(tool_registry);
    }

    /// Returns the brain_session_id. Panics if not yet set - callers from
    /// handler paths can rely on this because handlers fire only after
    /// `set_brain_session_id` has been called by the orchestrator (after
    /// `agent.new_session` returns the ACP session_id).
    pub fn brain_session_id(&self) -> &spur_acp::BrainSessionId {
        self.brain_session_id
            .get()
            .expect("brain_session_id must be set before MCP handlers dispatch")
    }

    fn submit_plan_substrate_pm(&self) -> Option<&dyn crate::plan::PmLike> {
        if let Some(pm) = self.pm_service_like.as_deref() {
            return Some(pm);
        }
        self.pm_service
            .as_deref()
            .map(|pm| pm as &dyn crate::plan::PmLike)
    }

    fn reconciler_pm(&self) -> Option<Arc<dyn crate::plan::PmLike>> {
        if let Some(pm) = self.pm_service_like.as_ref() {
            return Some(Arc::clone(pm));
        }
        self.pm_service
            .as_ref()
            .cloned()
            .map(|pm| pm as Arc<dyn crate::plan::PmLike>)
    }

    /// Wait until the orchestrator binds this callback server to its derived
    /// brain_session_id. JSON-RPC entry points use this defensively because
    /// some ACP agents connect MCP before replying to `new_session`.
    pub async fn brain_session_id_ready(&self) -> &spur_acp::BrainSessionId {
        loop {
            let notified = self.brain_session_id_notify.notified();
            tokio::pin!(notified);
            if let Some(id) = self.brain_session_id.get() {
                return id;
            }
            notified.await;
        }
    }

    /// Set the brain_session_id once. Idempotent on the same value; returns
    /// Err if already set to a different value.
    pub fn set_brain_session_id(
        self: &Arc<Self>,
        id: spur_acp::BrainSessionId,
    ) -> Result<(), spur_acp::BrainSessionId> {
        let result = if let Some(existing) = self.brain_session_id.get() {
            if existing == &id {
                self.brain_session_id_notify.notify_waiters();
                Ok(())
            } else {
                Err(id)
            }
        } else {
            match self.brain_session_id.set(id) {
                Ok(()) => {
                    self.brain_session_id_notify.notify_waiters();
                    Ok(())
                }
                Err(tokio::sync::SetError::AlreadyInitializedError(id))
                | Err(tokio::sync::SetError::InitializingError(id)) => Err(id),
            }
        };
        if result.is_ok() {
            Arc::clone(self).spawn_awaiting_review_rediscovery_if_ready();
            Arc::clone(self).spawn_startup_recovery_if_ready();
        }
        result
    }

    pub fn feature_gate(&self) -> Arc<spur_license::FeatureGate> {
        Arc::clone(&self.feature_gate)
    }

    pub fn require_feature(&self, key: FeatureKey) -> Result<(), McpError> {
        require_feature(key, self.feature_gate.as_ref())
    }

    fn require_feature_response(&self, id: Value, key: FeatureKey) -> Option<JsonRpcResponse> {
        self.require_feature(key)
            .err()
            .map(|error| JsonRpcResponse::mcp_error(id, error))
    }

    /// INV-6: Wire the orchestrator's `CancellationControl` handle into this
    /// server so `handle_cancel_delegation` can cancel active delegations
    /// directly rather than routing through the delegation channel.
    pub fn set_cancellation_control(&mut self, cc: CancellationControl) {
        self.cancellation_control = Some(cc);
    }

    /// Phase 1c: set how long `handle_delegate_to_worker` /
    /// `handle_delegate_parallel` wait inline for a worker's oneshot before
    /// falling through to the detached collector. Default is `0`
    /// (async-first); orchestrator wires this from
    /// `SpurConfig.delegation.inline_wait_ms` at server construction.
    pub fn set_inline_wait(&mut self, d: std::time::Duration) {
        self.inline_wait = d;
    }

    pub fn mark_retiring(&self) {
        self.retiring.store(true, Ordering::SeqCst);
    }

    pub fn cancel_in_flight_workers(&self) {
        self.cancel_token.cancel();
    }

    pub fn delegation_sender(&self) -> mpsc::Sender<DelegationRequest> {
        self.delegation_tx.clone()
    }

    pub fn workers_snapshot(&self) -> Vec<WorkerInfo> {
        self.workers.clone()
    }

    pub fn brain_session_id_cell(&self) -> Arc<OnceCell<spur_acp::BrainSessionId>> {
        Arc::clone(&self.brain_session_id)
    }

    pub fn active_delegations_handle(&self) -> Arc<tokio::sync::Mutex<HashSet<DelegationId>>> {
        Arc::clone(&self.active_delegations)
    }

    pub fn completed_delegations_handle(
        &self,
    ) -> Arc<tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>>
    {
        Arc::clone(&self.completed_delegations)
    }

    pub fn task_tracker_handle(&self) -> TaskTracker {
        self.task_tracker.clone()
    }

    pub fn cancellation_control_handle(&self) -> Option<CancellationControl> {
        self.cancellation_control.clone()
    }

    pub fn continuation_ctx_handle(&self) -> Arc<DetachedContinuationCtx> {
        Arc::clone(&self.continuation_ctx)
    }

    pub fn outcome_materializer(&self) -> OutcomeMaterializer {
        self.materializer.clone()
    }

    pub fn outcome_store_handle(&self) -> Arc<dyn spur_blob_store::OutcomeStore> {
        Arc::clone(&self.outcome_store)
    }

    pub fn inline_wait_duration(&self) -> std::time::Duration {
        self.inline_wait
    }

    pub fn retiring_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.retiring)
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    pub fn event_sink_handle(&self) -> Option<Arc<dyn spur_mcp::events::McpEventSink>> {
        self.event_sink.clone()
    }

    pub fn force_abort(&self) {
        self.task_tracker.close();
        self.root_shutdown_tx.lock().unwrap().take();
        let startup_recovery_handle = {
            let mut state = self.startup_recovery.lock().unwrap();
            state.pending = false;
            state.handle.take()
        };
        if let Some(handle) = startup_recovery_handle {
            handle.abort();
        }
        if let Some(handle) = self.reconciler_handle.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.root_handle.lock().unwrap().take() {
            handle.abort();
        }
    }

    /// v0a.3: Configure whether the reconciler should be spawned once the
    /// orchestrator binds `brain_session_id` and calls `enable_reconciler`.
    /// Must be called before `enable_reconciler`.
    ///
    /// - `enable` — when true, attempt to spawn the reconciler.
    /// - `fast_forward` — optional Notify channel. When provided, the reconciler ticks
    ///   immediately when notified (e.g., after a task completes).
    pub fn set_reconciler_enabled(
        &mut self,
        enable: bool,
        fast_forward: Option<Arc<tokio::sync::Notify>>,
    ) {
        self.reconciler_enabled = enable;
        self.reconciler_fast_forward = if enable {
            fast_forward.or_else(|| {
                self.reconciler_fast_forward
                    .as_ref()
                    .cloned()
                    .or_else(|| Some(Arc::new(tokio::sync::Notify::new())))
            })
        } else {
            None
        };
    }

    pub fn fast_forward_reconciler(&self) {
        notify_fast_forward(&self.reconciler_fast_forward);
    }

    /// Set the repository root path. Required for beads-backed startup and
    /// plan automation.
    pub fn set_repo_root(&mut self, root: std::path::PathBuf) {
        self.repo_root = Some(root);
    }

    /// Borrow the configured repo root, if any. Used by Phase 5 / Task 26
    /// when constructing a `WorkerMcpDeps` so worker MCP handlers can
    /// reconstruct diffs from persisted worker branches.
    pub fn repo_root(&self) -> Option<&std::path::Path> {
        self.repo_root.as_deref()
    }

    /// Clone-shared handle to the ephemeral reconciler outcome buffer.
    /// Used by Phase 5 / Task 26 to inject the buffer into the per-
    /// `BrainSession` `WorkerMcpServer` so `get_plan_status` reflects the
    /// reconciler view from the same brain.
    pub fn reconciler_outcomes_handle(
        &self,
    ) -> Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>> {
        Arc::clone(&self.reconciler_outcomes)
    }

    // ─── Orchestration-state extraction surface (Phase 4) ─────────────────
    //
    // These accessors expose the plan/reconciler orchestration-domain handles
    // so `spur_core::mcp::plan::PlanMcpDeps::from_server` can bundle them as
    // the input to the staged engine move into `spur-core`. They mirror the
    // delegation-extraction accessors above and add no behavior. See
    // `docs/superpowers/plans/2026-06-21-phase4-plan-reconciler-core-extraction.md`.

    /// Clone-shared handle to the versioned active-plan cache.
    pub fn active_plans_handle(&self) -> Arc<tokio::sync::Mutex<HashMap<String, CachedPlan>>> {
        Arc::clone(&self.active_plans)
    }

    /// Clone-shared handle to the `epic_id → plan_id` registry.
    pub fn plan_registry_handle(&self) -> Arc<tokio::sync::Mutex<crate::plan::PlanRegistry>> {
        Arc::clone(&self.plan_registry)
    }

    /// Clone-shared handle to the current-brain plan-ownership claim lock.
    pub fn plan_claim_lock_handle(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.active_plan_claim_lock)
    }

    /// Clone-shared test hook for forced persisted-plan version churn.
    pub fn version_churn_epic_for_test_handle(&self) -> Arc<tokio::sync::Mutex<Option<String>>> {
        Arc::clone(&self.version_churn_epic_for_test)
    }

    /// Optional PM service used by plan submission/projection.
    pub fn pm_service_handle(&self) -> Option<Arc<PmService>> {
        self.pm_service.clone()
    }

    /// Optional `PmLike` substrate handle used by the plan projector/reconciler.
    pub fn pm_like_handle(&self) -> Option<Arc<dyn crate::plan::PmLike>> {
        self.pm_service_like.clone()
    }

    /// Persisted-plan versioned-cache serving flag.
    pub fn versioned_cache_serve(&self) -> bool {
        self.versioned_cache_serve
    }

    /// PR3 non-advisory review-write flag.
    pub fn nonadvisory_review_writes(&self) -> bool {
        self.nonadvisory_review_writes
    }

    /// Reconciler-owned dispatch lease duration.
    pub fn dispatch_lease_duration(&self) -> std::time::Duration {
        self.dispatch_lease_duration
    }

    /// Opt-in auto-merge/PR on durable epic completion.
    pub fn auto_merge_approved_plans(&self) -> bool {
        self.auto_merge_approved_plans
    }

    /// Startup quarantine grace for stale `spur:plan-pending` epics.
    pub fn plan_pending_grace(&self) -> std::time::Duration {
        self.plan_pending_grace
    }

    /// Whether the beads reconciler is enabled for this server.
    pub fn reconciler_enabled(&self) -> bool {
        self.reconciler_enabled
    }

    /// v0e: opt-in auto-merge/PR on durable epic completion.
    pub fn set_auto_merge_approved_plans(&mut self, enabled: bool) {
        self.auto_merge_approved_plans = enabled;
    }

    /// Configure startup quarantine grace for stale `spur:plan-pending` epics.
    pub fn set_plan_pending_grace(&mut self, grace: std::time::Duration) {
        self.plan_pending_grace = grace;
    }

    /// Configure persisted-plan cache serving.
    pub fn set_versioned_cache_serve(&mut self, enabled: bool) {
        self.versioned_cache_serve = enabled;
    }

    /// Configure PR3 non-advisory review writes. Default is off for staged
    /// production rollout; dev configs can opt in explicitly.
    pub fn set_nonadvisory_review_writes(&mut self, enabled: bool) {
        self.nonadvisory_review_writes = enabled;
    }

    /// Configure persisted dispatch lease duration for reconciler dispatches.
    pub fn set_dispatch_lease_duration(&mut self, duration: std::time::Duration) {
        self.dispatch_lease_duration = duration;
    }

    /// Set the list of available worker agents.
    pub fn set_workers(&mut self, workers: Vec<WorkerInfo>) {
        self.workers = workers;
    }

    /// Gracefully shut down the server: close the task tracker and wait
    /// for all in-flight result collectors to finish.
    pub async fn shutdown(&self) {
        self.task_tracker.close();
        if let Some(tx) = self.root_shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let startup_recovery_handle = {
            let mut state = self.startup_recovery.lock().unwrap();
            state.pending = false;
            state.handle.take()
        };
        if let Some(handle) = startup_recovery_handle {
            handle.shutdown().await;
        }
        let reconciler_handle = self.reconciler_handle.lock().unwrap().take();
        if let Some(handle) = reconciler_handle {
            handle.shutdown().await;
        }
        self.task_tracker.wait().await;
    }

    /// Spawn the beads reconciler after the orchestrator has bound the derived
    /// brain_session_id. Safe no-op when the reconciler is disabled, the PM
    /// backend is absent/non-beads, or the advanced beads feature is gated off.
    pub async fn enable_reconciler(self: Arc<Self>) -> Result<()> {
        if !self.reconciler_enabled {
            tracing::debug!("reconciler disabled: reconciler_enabled = false");
            return Ok(());
        }
        if self.reconciler_handle.lock().unwrap().is_some() {
            return Ok(());
        }

        let Some(pm) = self.reconciler_pm() else {
            tracing::debug!("reconciler disabled: no PM service");
            return Ok(());
        };
        if self
            .require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
            .is_err()
            || pm.advanced().is_none()
        {
            tracing::debug!("reconciler disabled: PM has no beads advanced() backend");
            return Ok(());
        }

        let repo_root = self
            .repo_root
            .clone()
            .context("repo_root not set on McpCallbackServer")?;
        let brain_session_id = self
            .brain_session_id
            .get()
            .cloned()
            .context("brain_session_id must be set before enabling reconciler")?;
        let fast_forward = self
            .reconciler_fast_forward
            .as_ref()
            .cloned()
            .expect("reconciler_enabled must retain a fast-forward notify");

        let dispatch = Arc::new(ReconcilerDispatchCtx {
            delegation_tx: self.delegation_tx.clone(),
            task_tracker: self.task_tracker.clone(),
            brain_session_id,
            event_sink: self.event_sink.clone(),
            materializer: Arc::new(self.materializer.clone()),
            continuation_ctx: Arc::clone(&self.continuation_ctx),
        }) as Arc<dyn ReconcilerDispatch>;
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        info!("spawning plan reconciler (beads backend detected)");
        let auto_merge = self.auto_merge_approved_plans;
        let reconciler_config = ReconcilerConfig {
            dispatch_lease_duration: self.dispatch_lease_duration,
            repo_root: repo_root.clone(),
            ..Default::default()
        };
        let automation: Option<Arc<dyn crate::plan::reconciler::ReconcilerAutomation>> =
            Some(Arc::new(self.plan_mcp_deps())
                as Arc<dyn crate::plan::reconciler::ReconcilerAutomation>);
        let feature_gate = Arc::clone(&self.feature_gate);
        let reconciler_outcomes = Arc::clone(&self.reconciler_outcomes);
        let journal_notify = Arc::new(tokio::sync::Notify::new());
        let journal_handle = {
            let path = crate::plan::reconciler::beads_journal_path(&repo_root);
            AbortOnDropHandle::new(tokio::spawn(
                crate::plan::reconciler::monitor_journal_appends(path, Arc::clone(&journal_notify)),
            ))
        };
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut reconciler = Reconciler::new_with_pm_like(
                reconciler_config,
                pm,
                fast_forward,
                Some(dispatch),
                None,
                feature_gate,
            );
            reconciler.set_outcomes(reconciler_outcomes);
            reconciler.set_auto_merge_approved_plans(auto_merge);
            reconciler.set_journal_wake(journal_notify);
            if let Some(a) = automation {
                reconciler.set_automation(a);
            }
            reconciler.run(cancel_rx).await;
            drop(journal_handle);
        }));

        let mut task_handle = Some(ReconcilerTaskHandle {
            cancel_tx: Some(cancel_tx),
            handle,
        });
        {
            let mut guard = self.reconciler_handle.lock().unwrap();
            if guard.is_some() {
                if let Some(handle) = task_handle.take() {
                    handle.abort();
                }
                return Ok(());
            }
            *guard = task_handle.take();
        }

        let has_active_plans = !self.active_plans.lock().await.is_empty();
        tokio::task::yield_now().await;
        if has_active_plans {
            self.fast_forward_reconciler();
        }

        Ok(())
    }

    /// Start listening on a random localhost port.
    ///
    /// Returns the MCP endpoint URL (e.g. `http://127.0.0.1:12345/mcp`) and
    /// a `JoinHandle`.
    pub async fn start(self: Arc<Self>) -> Result<(String, AbortOnDropHandle<()>)> {
        spur_analyst::mcp::warm_embed_model();

        // Extract data needed by beads-backed startup tasks before moving self
        // into the async block.
        let repo_root = self.repo_root.clone();
        let has_beads_backend = self
            .pm_service
            .as_ref()
            .map(|pm| {
                self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
                    .is_ok()
                    && pm.advanced().is_some()
            })
            .unwrap_or(false);

        if has_beads_backend && repo_root.is_none() {
            anyhow::bail!("repo_root not set on McpCallbackServer");
        }

        if let Some(pm) = self.pm_service.as_ref() {
            if self
                .require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
                .is_ok()
                && pm.advanced().is_some()
            {
                self.request_startup_recovery();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind TCP listener")?;

        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/mcp");
        let mut config = StreamableHttpServerConfig::default();
        config.stateful_mode = true;
        let mut session_manager_inner = LocalSessionManager::default();
        session_manager_inner.session_config.keep_alive = mcp_session_keepalive();
        let session_manager = Arc::new(session_manager_inner);
        let service = {
            let server = Arc::clone(&self);
            StreamableHttpService::new(move || Ok(Arc::clone(&server)), session_manager, config)
        };
        let router = Router::new().nest_service("/mcp", service);

        let mut signal_watcher_cancel_tx: Option<tokio::sync::oneshot::Sender<()>> = None;
        let signal_watcher_task = if let Some(pm) = self.pm_service.as_ref() {
            if self
                .require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
                .is_ok()
                && pm.advanced().is_some()
            {
                let pm = Arc::clone(pm);
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                signal_watcher_cancel_tx = Some(cancel_tx);
                info!("spawning brain-side signal watcher (beads backend detected)");
                let feature_gate = Arc::clone(&self.feature_gate);
                let handle = AbortOnDropHandle::new(tokio::spawn(async move {
                    // See docs/superpowers/plans/2026-06-13-scope-drift-escalation-fix.md.
                    let proposer = CompositeProposer::new(vec![Box::new(RetryExhaustedProposer)]);
                    let watcher = SignalWatcher::new(pm, proposer, TrivialScorer, feature_gate);
                    watcher.run(cancel_rx).await;
                }));
                Some(handle)
            } else {
                tracing::debug!("signal watcher disabled: PM has no beads advanced() backend");
                None
            }
        } else {
            tracing::debug!("signal watcher disabled: no PM service");
            None
        };

        info!(url = %url, "MCP callback server listening (streamable HTTP)");

        let (root_shutdown_tx, root_shutdown_rx) = tokio::sync::oneshot::channel();
        let (root_done_tx, root_done_rx) = tokio::sync::oneshot::channel();
        let root_handle = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = root_shutdown_rx.await;
                })
                .await
            {
                debug!(%error, "RMCP callback server exited");
            }
            if let Some(tx) = signal_watcher_cancel_tx {
                let _ = tx.send(());
            }
            if let Some(sh) = signal_watcher_task {
                let _ = sh.await;
            }
            let _ = root_done_tx.send(());
        });
        *self.root_shutdown_tx.lock().unwrap() = Some(root_shutdown_tx);
        *self.root_handle.lock().unwrap() = Some(root_handle);
        Arc::clone(&self).spawn_startup_recovery_if_ready();

        let server_for_drop = Arc::clone(&self);
        struct AbortRootOnDrop {
            server: Arc<McpCallbackServer>,
        }

        impl Drop for AbortRootOnDrop {
            fn drop(&mut self) {
                let startup_recovery_handle = {
                    let mut state = self.server.startup_recovery.lock().unwrap();
                    state.pending = false;
                    state.handle.take()
                };
                if let Some(handle) = startup_recovery_handle {
                    handle.abort();
                }
                if let Some(handle) = self.server.reconciler_handle.lock().unwrap().take() {
                    handle.abort();
                }
                self.server.root_shutdown_tx.lock().unwrap().take();
                if let Some(handle) = self.server.root_handle.lock().unwrap().take() {
                    handle.abort();
                }
            }
        }

        let drop_guard = AbortRootOnDrop {
            server: server_for_drop,
        };
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            let _guard = drop_guard;
            let _ = root_done_rx.await;
        }));

        Ok((url, handle))
    }

    pub(crate) async fn check_plan_owner_for_op(
        &self,
        plan_id: &str,
        op_name: &str,
    ) -> Result<(), (i64, String)> {
        self.plan_mcp_deps()
            .check_plan_owner_for_op(plan_id, op_name)
            .await
    }

    async fn is_projected_plan_nonterminal(&self, plan_id: &str) -> Result<bool, String> {
        self.plan_mcp_deps()
            .is_projected_plan_nonterminal(plan_id)
            .await
    }

    /// Single-active-plan-per-brain quota check. Layered ON TOP of plan-scoped
    /// ownership: this assumes ownership labels are already maintained correctly
    /// (per main's plan-scoped system) and enforces that any one brain holds at
    /// most one non-terminal owned plan at a time.
    async fn current_brain_active_owned_plan(
        &self,
        pm: &dyn crate::plan::PmLike,
        exempt_plan_id: Option<&str>,
        exempt_epic_id: Option<&str>,
    ) -> Result<Option<ActiveOwnedPlan>, String> {
        self.plan_mcp_deps()
            .current_brain_active_owned_plan(pm, exempt_plan_id, exempt_epic_id)
            .await
    }

    async fn nonterminal_plan_status_for_epic(
        &self,
        pm: &dyn crate::plan::PmLike,
        epic_id: &str,
    ) -> Result<Option<(String, serde_json::Value)>, String> {
        self.plan_mcp_deps()
            .nonterminal_plan_status_for_epic(pm, epic_id)
            .await
    }

    async fn install_projected_plan(&self, projected: crate::plan::PlanState, emit_snapshot: bool) {
        self.plan_mcp_deps()
            .install_projected_plan(projected, emit_snapshot)
            .await;
    }

    #[cfg(test)]
    async fn derive_beads_version(
        pm: &spur_pm::PmService,
        feature_gate: &spur_license::FeatureGate,
        epic_id: &str,
    ) -> Result<BeadsVersion, String> {
        plan_deps::PlanMcpDeps::derive_beads_version(pm, feature_gate, epic_id).await
    }

    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        self.plan_mcp_deps().load_or_project_plan(plan_id).await
    }

    #[cfg(test)]
    async fn load_or_project_plan_with_freshness(
        &self,
        plan_id: &str,
    ) -> Result<crate::handlers::ResolvedPlanState, String> {
        self.plan_mcp_deps()
            .load_or_project_plan_with_freshness(plan_id)
            .await
    }
}

#[async_trait::async_trait]
impl crate::plan::reconciler::ReconcilerAutomation for McpCallbackServer {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
        self.plan_mcp_deps().merge_plan_impl(plan_id).await
    }

    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.plan_mcp_deps().create_pr_impl(params).await
    }
}

#[async_trait::async_trait]
impl crate::handlers::PlanResolver for McpCallbackServer {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        self.plan_mcp_deps().load_or_project_plan(plan_id).await
    }

    async fn load_or_project_plan_with_freshness(
        &self,
        plan_id: &str,
    ) -> Result<crate::handlers::ResolvedPlanState, String> {
        self.plan_mcp_deps()
            .load_or_project_plan_with_freshness(plan_id)
            .await
    }
}

impl ServerHandler for McpCallbackServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Use these tools to delegate work, inspect plan status, and interact with the configured project management backend."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut implementation = Implementation::default();
        implementation.name = "spur-mcp".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tool(name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.rmcp_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.to_string();
        let params = json!({
            "name": tool_name,
            "arguments": request.arguments.map(Value::Object).unwrap_or_else(|| json!({})),
        });
        let response = self.handle_tool_call(Value::Null, params).await;
        Self::call_tool_result_from_legacy_response(response, &tool_name)
    }
}

#[cfg(test)]
mod tests;
