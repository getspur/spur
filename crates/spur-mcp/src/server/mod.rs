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
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};
use tracing::{debug, error, info};

use spur_acp::*;
use spur_license::FeatureKey;
use spur_pm::{IssueFilter, IssueSummary, IssueUpdate, PmService, PrParams};
use spur_worktree::WorktreeManager;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::proposers::{
    CompositeProposer, RetryExhaustedProposer, ScopeDriftSplitProposer, TrivialScorer,
};
use crate::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use crate::plan::signal_watcher::SignalWatcher;
use crate::tools::{self, DelegationChannel, DelegationRequest};

pub(crate) mod handlers;
pub(crate) mod plan_builder;
pub(crate) mod recovery;
pub(crate) mod review;
pub(crate) mod sync;
pub(crate) mod test_helpers;
pub(crate) mod types;

pub(crate) use plan_builder::*;
pub use plan_builder::{
    build_entries_with_task_map, build_epic_subgraph, emit_plan_submit_audit,
    plan_epic_issue_creates, EpicSubgraph, PlanSubmitAuditContext,
};
pub(crate) use sync::*;
pub use sync::{compensate_mutation_orphans, resolve_dispatch_orphan};
pub use types::*;

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
    pub(crate) event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
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
    pub(crate) retiring: AtomicBool,
    /// v3-c: parent cancellation token for in-flight collector tasks.
    pub(crate) cancel_token: CancellationToken,
    /// v3-c: handle to the root listener task so `force_abort` can stop it.
    pub(crate) root_handle: Mutex<Option<JoinHandle<()>>>,
    /// Handle to the optional beads reconciler task. It is enabled only after
    /// the orchestrator binds this server to a derived brain_session_id.
    pub(crate) reconciler_handle: Mutex<Option<ReconcilerTaskHandle>>,
    /// Optional startup recovery for legacy persisted plans. `start()` only
    /// decides whether it is needed; the task is spawned after the server has
    /// a brain_session_id so recovery can be owner-aware.
    pub(crate) startup_recovery: Mutex<StartupRecoveryState>,
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
}

impl McpCallbackServer {
    /// Create a new MCP callback server.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(
        session_id: Option<&spur_acp::BrainSessionId>,
        pm_service: Option<Arc<PmService>>,
        event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
        continuation_ctx: DetachedContinuationCtx,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> (Self, DelegationChannel) {
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);
        let materializer = OutcomeMaterializer::new(outcome_store.clone());

        let server = Self {
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
            retiring: AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            root_handle: Mutex::new(None),
            reconciler_handle: Mutex::new(None),
            startup_recovery: Mutex::new(StartupRecoveryState::default()),
            reconciler_enabled: false,
            reconciler_fast_forward: None,
            repo_root: None,
            auto_merge_approved_plans: false,
            plan_pending_grace: DEFAULT_PLAN_PENDING_GRACE,
            versioned_cache_serve: false,
            nonadvisory_review_writes: false,
            dispatch_lease_duration: std::time::Duration::from_secs(600),
        };

        let channel = DelegationChannel { request_rx: req_rx };
        (server, channel)
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

    pub fn force_abort(&self) {
        self.task_tracker.close();
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

        let dispatch = ReconcilerDispatchCtx {
            delegation_tx: self.delegation_tx.clone(),
            task_tracker: self.task_tracker.clone(),
            brain_session_id,
            event_sink: self.event_sink.clone(),
            materializer: Arc::new(self.materializer.clone()),
            continuation_ctx: Arc::clone(&self.continuation_ctx),
        };
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        info!("spawning plan reconciler (beads backend detected)");
        let auto_merge = self.auto_merge_approved_plans;
        let reconciler_config = ReconcilerConfig {
            dispatch_lease_duration: self.dispatch_lease_duration,
            repo_root: repo_root.clone(),
            ..Default::default()
        };
        let automation: Option<Arc<dyn crate::plan::reconciler::ReconcilerAutomation>> =
            Some(Arc::clone(&self) as Arc<dyn crate::plan::reconciler::ReconcilerAutomation>);
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
                    // bd-2m2u Phase 2e — fan out signals to both the
                    // ScopeDrift split proposer (v0b) and the RetryExhausted
                    // recovery proposer (v0e). Each inner proposer no-ops on
                    // unmatched signal kinds, so the composite dispatch is
                    // safe and ordering-insensitive.
                    let proposer = CompositeProposer::new(vec![
                        Box::new(ScopeDriftSplitProposer::default()),
                        Box::new(RetryExhaustedProposer),
                    ]);
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

        let (root_done_tx, root_done_rx) = tokio::sync::oneshot::channel();
        let root_handle = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
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

    async fn check_plan_owner_for_op(
        &self,
        plan_id: &str,
        op_name: &str,
    ) -> Result<(), (i64, String)> {
        let Some(pm) = self.pm_service.as_deref() else {
            return Ok(());
        };

        let epics = pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await
            .map_err(|error| (-32603, format!("{op_name}: failed to find plan: {error}")))?;

        let Some(epic_summary) = epics.first() else {
            return Err((-32004, format!("{op_name}: plan not found: {plan_id}")));
        };
        if epics.len() > 1 {
            let epic_ids = epics
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err((
                -32009,
                format!(
                    "{op_name}: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
                ),
            ));
        }
        let epic_id = epic_summary.id.clone();
        let epic = pm.get_issue(&epic_id).await.map_err(|error| {
            (
                -32603,
                format!("{op_name}: failed to load epic {epic_id}: {error}"),
            )
        })?;

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Ok(()),
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} is owned by {owner}; active handoff is not implemented in MVP"
                ),
            )),
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} has ambiguous owner labels: {}",
                    owners.join(", ")
                ),
            )),
            crate::plan::ownership::PlanOwnerMatch::Unowned => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} is unowned; claim it via execute_epic or resume_plan first"
                ),
            )),
        }
    }

    async fn projected_plan_status(&self, plan_id: &str) -> Result<serde_json::Value, String> {
        let plan_arc = self.load_or_project_plan(plan_id).await?;
        let state = plan_arc.lock().await;
        Ok(crate::plan::build_plan_status(plan_id, &state))
    }

    async fn is_projected_plan_nonterminal(&self, plan_id: &str) -> Result<bool, String> {
        let status = self.projected_plan_status(plan_id).await?;
        let overall = status
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        Ok(!crate::plan::is_terminal_plan_status(overall))
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
        let owner_label =
            crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
        let epics = pm
            .list_issues(IssueFilter {
                labels: vec![owner_label],
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                include_closed: false,
                limit: Some(10_000),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("failed to scan active owned plans: {error}"))?;

        for epic_summary in epics {
            let epic_id = epic_summary.id;
            let epic = pm
                .get_issue(&epic_id)
                .await
                .map_err(|error| format!("failed to load owned plan epic {epic_id}: {error}"))?;
            let plan_ids = epic
                .labels
                .iter()
                .filter_map(|label| crate::plan::labels::parse_plan_id(label))
                .collect::<HashSet<_>>();

            for plan_id in plan_ids {
                if exempt_plan_id == Some(plan_id) || exempt_epic_id == Some(epic_id.as_str()) {
                    continue;
                }
                if self.is_projected_plan_nonterminal(plan_id).await? {
                    return Ok(Some(ActiveOwnedPlan {
                        plan_id: plan_id.to_string(),
                        epic_id: epic_id.clone(),
                    }));
                }
            }
        }

        Ok(None)
    }

    async fn nonterminal_plan_status_for_epic(
        &self,
        pm: &dyn crate::plan::PmLike,
        epic_id: &str,
    ) -> Result<Option<(String, serde_json::Value)>, String> {
        let epic = pm
            .get_issue(epic_id)
            .await
            .map_err(|error| format!("failed to load epic {epic_id}: {error}"))?;
        let plan_ids = epic
            .labels
            .iter()
            .filter_map(|label| crate::plan::labels::parse_plan_id(label))
            .collect::<HashSet<_>>();
        let mut active = Vec::new();

        for plan_id in plan_ids {
            let status = self.projected_plan_status(plan_id).await?;
            let overall = status
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            if !crate::plan::is_terminal_plan_status(overall) {
                active.push((plan_id.to_string(), status));
            }
        }

        match active.len() {
            0 => Ok(None),
            1 => Ok(active.into_iter().next()),
            _ => Err(format!(
                "epic {epic_id} has multiple nonterminal plans: {}",
                active
                    .iter()
                    .map(|(plan_id, _)| plan_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    async fn install_projected_plan(&self, projected: crate::plan::PlanState, emit_snapshot: bool) {
        let plan_id = projected.plan_id.clone();
        if let Some(epic_id) = projected.epic_id.clone() {
            self.plan_registry
                .lock()
                .await
                .by_epic
                .insert(epic_id, plan_id.clone());
        }
        if emit_snapshot {
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &projected);
        }
        self.install_projected_plan_with_version(projected, unknown_beads_version())
            .await;
    }

    async fn install_projected_plan_with_version(
        &self,
        projected: crate::plan::PlanState,
        beads_version: BeadsVersion,
    ) -> Arc<tokio::sync::Mutex<crate::plan::PlanState>> {
        let plan_id = projected.plan_id.clone();
        if let Some(epic_id) = projected.epic_id.clone() {
            self.plan_registry
                .lock()
                .await
                .by_epic
                .insert(epic_id, plan_id.clone());
        }
        let state = Arc::new(tokio::sync::Mutex::new(projected));
        self.active_plans
            .lock()
            .await
            .insert(plan_id, CachedPlan::new(Arc::clone(&state), beads_version));
        state
    }

    async fn maybe_churn_beads_version_for_test(&self, epic_id: &str) -> Result<(), String> {
        let churn_epic = self.version_churn_epic_for_test.lock().await.clone();
        if churn_epic.as_deref() != Some(epic_id) {
            return Ok(());
        }
        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| "test version churn requires PM service".to_string())?;
        require_feature(
            FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(feature_error_message)?;
        let advanced = pm
            .advanced()
            .ok_or_else(|| "test version churn requires beads advanced backend".to_string())?;
        advanced
            .add_comment(
                epic_id,
                &crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                        plan_id: "test-version-churn".into(),
                        owner: "test".into(),
                        token: uuid::Uuid::new_v4().to_string(),
                        reason: "versioned-cache-retry-bound".into(),
                    },
                ),
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("test version churn failed: {error}"))
    }

    async fn beads_version_for_epic(&self, epic_id: &str) -> Result<BeadsVersion, String> {
        self.maybe_churn_beads_version_for_test(epic_id).await?;
        let pm = self.pm_service.as_deref().ok_or_else(|| {
            format!("beads version unavailable for epic '{epic_id}': PM service not configured")
        })?;
        Self::derive_beads_version(pm, self.feature_gate.as_ref(), epic_id).await
    }

    async fn derive_beads_version(
        pm: &spur_pm::PmService,
        feature_gate: &spur_license::FeatureGate,
        epic_id: &str,
    ) -> Result<BeadsVersion, String> {
        require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate,
        )
        .map_err(feature_error_message)?;
        let adv = pm
            .advanced()
            .ok_or_else(|| "beads version derivation requires beads backend".to_string())?;
        let comments = adv
            .list_comments(epic_id)
            .await
            .map_err(|error| format!("list_comments({epic_id}) failed: {error}"))?;
        let epic_issue = pm
            .get_issue(epic_id)
            .await
            .map_err(|error| format!("get_issue({epic_id}) failed: {error}"))?;
        let plan_id = epic_issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_plan_id(label));
        let Some(plan_id) = plan_id else {
            return Ok(BeadsVersion::AuditSeq(
                crate::plan::projector::sort_projection_comments(comments)
                    .into_iter()
                    .filter(|comment| {
                        comment
                            .body
                            .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX)
                    })
                    .count() as u64,
            ));
        };

        // Option B (content-addressed): derive a cache token from the sorted
        // set of plan-scoped audit comment IDs. This avoids additive-count
        // collisions across plan restarts and aligns issue discovery with the
        // projector (scan by `spur:plan-id:<id>` label).
        let mut summary_by_id = HashMap::new();
        for status in [
            Some("open".to_string()),
            Some("in_progress".to_string()),
            Some(pm.closed_status().to_string()),
        ] {
            for summary in pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    status,
                    limit: Some(1_000),
                    ..Default::default()
                })
                .await
                .map_err(|error| format!("list_issues(plan={plan_id}) failed: {error}"))?
            {
                summary_by_id.insert(summary.id.clone(), summary);
            }
        }
        let mut issue_ids: Vec<String> = summary_by_id.into_keys().collect();
        issue_ids.sort();

        let comments_by_issue =
            futures::future::try_join_all(issue_ids.iter().map(|issue_id| async move {
                adv.list_comments(issue_id)
                    .await
                    .map(|comments| (issue_id.clone(), comments))
            }))
            .await
            .map_err(|error| format!("list_comments(plan={plan_id}) failed: {error}"))?;

        let mut audit_keys = Vec::new();
        for (issue_id, comments) in comments_by_issue {
            for comment in crate::plan::projector::sort_projection_comments(comments) {
                if !comment
                    .body
                    .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX)
                {
                    continue;
                }
                if let Some(Err(error)) = crate::plan::audit_sentinel::parse_comment(&comment.body)
                {
                    tracing::warn!(
                        %plan_id,
                        %issue_id,
                        comment_id = %comment.id,
                        %error,
                        "malformed audit sentinel included in beads version hash"
                    );
                }
                audit_keys.push((issue_id.clone(), comment.id));
            }
        }
        audit_keys.sort();

        let mut hasher = Sha256::new();
        for (issue_id, comment_id) in audit_keys {
            hasher.update(issue_id.as_bytes());
            hasher.update([0_u8]);
            hasher.update(comment_id.as_bytes());
            hasher.update([0_u8]);
        }
        let digest = hasher.finalize();
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(&digest);
        Ok(BeadsVersion::ContentHash(hash))
    }

    async fn project_plan_from_beads_with_stable_version(
        &self,
        pm: &spur_pm::PmService,
        plan_id: &str,
    ) -> Result<(crate::plan::PlanState, BeadsVersion), String> {
        let epic = find_plan_epic(
            pm,
            self.feature_gate.as_ref(),
            plan_id,
            "load_or_project_plan",
        )
        .await?;

        for (attempt, backoff) in VERSIONED_PLAN_CACHE_BACKOFFS.iter().enumerate() {
            let before_version = self.beads_version_for_epic(&epic.id).await?;
            let projected = crate::plan::projector::project_plan_from_beads(
                pm,
                plan_id,
                self.feature_gate.as_ref(),
            )
            .await
            .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
            let after_version = self.beads_version_for_epic(&epic.id).await?;
            if before_version == after_version {
                return Ok((projected, after_version));
            }

            tracing::debug!(
                %plan_id,
                epic_id = %epic.id,
                attempt = attempt + 1,
                before_version = ?before_version,
                after_version = ?after_version,
                "persisted plan changed during projection; retrying"
            );

            if attempt + 1 < VERSIONED_PLAN_CACHE_MAX_ATTEMPTS {
                tokio::time::sleep(*backoff).await;
            }
        }

        Err(format!(
            "load_or_project_plan: plan '{plan_id}' changed during projection after {VERSIONED_PLAN_CACHE_MAX_ATTEMPTS} attempts"
        ))
    }

    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        let cached = self.active_plans.lock().await.get(plan_id).cloned();
        if let Some(existing) = cached.clone() {
            let epic_id = {
                let state = existing.state.lock().await;
                state.epic_id.clone()
            };
            if let Some(epic_id) = epic_id {
                if self.versioned_cache_serve {
                    let current_version = self.beads_version_for_epic(&epic_id).await?;
                    if current_version == existing.beads_version {
                        return Ok(existing.state);
                    }
                    tracing::debug!(
                        %plan_id,
                        %epic_id,
                        cached_age_ms = existing.cached_at.elapsed().as_millis(),
                        cached_version = ?existing.beads_version,
                        current_version = ?current_version,
                        "persisted plan cache version mismatch; re-projecting from beads"
                    );
                }
            }
        }

        let pm = self
            .submit_plan_substrate_pm()
            .ok_or_else(|| format!("unknown plan '{plan_id}'"))?;
        if self.versioned_cache_serve {
            if let Some(pm_service) = self.pm_service.as_deref() {
                let (projected, beads_version) = self
                    .project_plan_from_beads_with_stable_version(pm_service, plan_id)
                    .await?;
                return Ok(self
                    .install_projected_plan_with_version(projected, beads_version)
                    .await);
            }
        }

        let projected = crate::plan::projector::project_plan_from_beads(
            pm,
            plan_id,
            self.feature_gate.as_ref(),
        )
        .await
        .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
        Ok(self
            .install_projected_plan_with_version(projected, unknown_beads_version())
            .await)
    }
}

#[async_trait::async_trait]
impl crate::plan::reconciler::ReconcilerAutomation for McpCallbackServer {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
        self.merge_plan_impl(plan_id).await
    }

    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.create_pr_impl(params).await
    }
}

#[async_trait::async_trait]
impl crate::handlers::PlanResolver for McpCallbackServer {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        McpCallbackServer::load_or_project_plan(self, plan_id).await
    }
}

pub(crate) fn notify_fast_forward(fast_forward: &Option<Arc<tokio::sync::Notify>>) {
    if let Some(notify) = fast_forward {
        notify.notify_one();
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
