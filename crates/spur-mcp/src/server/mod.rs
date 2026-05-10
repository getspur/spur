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

    fn request_startup_recovery(&self) {
        let mut state = self.startup_recovery.lock().unwrap();
        if state.handle.is_none() {
            state.pending = true;
        }
    }

    /// Spawn legacy persisted-plan recovery after the brain session id is
    /// available. Safe no-op when startup did not request recovery, when the
    /// task is already running, or when the brain has not been bound yet.
    #[doc(hidden)]
    pub fn spawn_startup_recovery_if_ready(self: Arc<Self>) {
        let mut state = self.startup_recovery.lock().unwrap();
        if !state.pending || state.handle.is_some() {
            return;
        }
        if self.task_tracker.is_closed() {
            state.pending = false;
            return;
        }
        if self.brain_session_id.get().is_none() {
            return;
        }
        let Some(pm) = self.pm_service.as_ref().cloned() else {
            state.pending = false;
            return;
        };
        if self
            .require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
            .is_err()
            || pm.advanced().is_none()
        {
            state.pending = false;
            return;
        }

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let server = Arc::clone(&self);
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel_rx => {
                    tracing::debug!("persisted-plan startup recovery cancelled");
                    return;
                }
                result = server.reclaim_persisted_plans_on_startup(pm) => result,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "persisted-plan startup recovery failed");
            }
        }));

        state.pending = false;
        state.handle = Some(StartupRecoveryTaskHandle {
            cancel_tx: Some(cancel_tx),
            handle,
        });
    }

    /// Return the feature gate snapshot shared with the license runtime.
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

    /// Configure PR2 persisted-plan cache serving. Default is off until PR3
    /// makes task-level durable writes advance the epic audit sequence.
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

    async fn merge_plan_impl(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
        let repo_root = match self.repo_root.as_ref() {
            Some(root) => root.clone(),
            None => anyhow::bail!("Repository root not configured; merge_plan is unavailable"),
        };

        let had_cached_entry = self.active_plans.lock().await.contains_key(plan_id);
        let plan_arc = match self.load_or_project_plan(plan_id).await {
            Ok(plan_arc) => plan_arc,
            Err(_) => anyhow::bail!("Unknown plan_id: '{plan_id}'"),
        };

        let (base_snapshot_branch, base_snapshot_oid, tasks, merge_state, epic_id) = {
            let state = plan_arc.lock().await;
            let status = crate::plan::build_plan_status(plan_id, &state);
            let ready = status
                .get("ready_to_merge")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !ready {
                anyhow::bail!("plan '{plan_id}' is not fully approved yet");
            }
            (
                state.base_snapshot_branch.clone(),
                state.base_snapshot_oid.clone(),
                state.tasks.clone(),
                state.merge_state.clone(),
                state.epic_id.clone(),
            )
        };

        if !matches!(merge_state, crate::plan::PlanMergeState::NotStarted) {
            return Ok(merge_state);
        }

        let persisted_bootstrap = if !had_cached_entry {
            match (self.pm_service.as_deref(), epic_id.as_deref()) {
                (Some(pm), Some(epic_id)) => {
                    match read_persisted_plan_bootstrap(
                        pm,
                        self.feature_gate.as_ref(),
                        plan_id,
                        epic_id,
                    )
                    .await
                    {
                        Ok(bootstrap) => Some(bootstrap),
                        Err(error) => anyhow::bail!(error),
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        let base_snapshot_ref = match persisted_bootstrap
            .as_ref()
            .and_then(PersistedPlanBootstrap::preferred_base_ref)
            .map(str::to_string)
            .or(base_snapshot_oid)
            .or(base_snapshot_branch)
        {
            Some(reference) => reference,
            None => anyhow::bail!(
                "plan '{plan_id}' has no captured base snapshot; resubmit the plan before calling merge_plan"
            ),
        };

        let task_specs: Vec<crate::plan::PlanTask> =
            tasks.iter().map(|entry| entry.spec.clone()).collect();
        let order = topological_order(&task_specs).map_err(|e| anyhow::anyhow!(e))?;

        let mut ordered_branches = Vec::with_capacity(order.len());
        for idx in order {
            let entry = &tasks[idx];
            if !matches!(entry.status, crate::plan::PlanTaskStatus::Approved { .. }) {
                anyhow::bail!(
                    "plan '{plan_id}' became non-approved while merge_plan was preparing task '{}'",
                    entry.spec.task_id
                );
            }
            let Some(worker_branch) = entry.worker_branch.clone() else {
                anyhow::bail!(
                    "approved task '{}' has no worker_branch; cannot integrate plan",
                    entry.spec.task_id
                );
            };
            ordered_branches.push((entry.spec.task_id.clone(), worker_branch));
        }

        let merge_branch = format!(
            "spur/plan-merge-{plan_id}-{}",
            uuid::Uuid::new_v4().simple()
        );

        let merge_state = match integrate_plan_branches(
            &repo_root,
            &base_snapshot_ref,
            &merge_branch,
            &ordered_branches,
        )
        .await
        {
            Ok(state) => state,
            Err(error) => crate::plan::PlanMergeState::Failed { error },
        };
        let merged_successfully =
            matches!(merge_state, crate::plan::PlanMergeState::Succeeded { .. });

        {
            let mut state = plan_arc.lock().await;
            state.merge_state = merge_state.clone();
        }

        if merged_successfully {
            if let (Some(pm), Some(epic_id)) = (self.pm_service.as_ref(), epic_id.as_deref()) {
                if let Err(error) = apply_issue_update(
                    pm.as_ref(),
                    epic_id,
                    spur_pm::IssueUpdate {
                        remove_labels: vec![crate::plan::labels::INTEGRATION_PENDING.to_string()],
                        ..Default::default()
                    },
                )
                .await
                {
                    tracing::warn!(
                        plan_id = %plan_id,
                        epic_id = %epic_id,
                        "failed to clear integration-pending on epic after merge: {error}"
                    );
                }
            }
        }

        Ok(merge_state)
    }

    async fn create_pr_impl(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.pm_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No PR service configured"))?
            .create_pr(params)
            .await
    }

    /// Refuse the operation unless the current brain owns the epic for `plan_id`.
    ///
    /// Returns `Ok(())` only when (a) PM service is unavailable (in-memory-only
    /// paths have no durable epic to gate on, so we stay permissive) or
    /// (b) the epic exists and `classify_owner` resolves to `OwnedByCurrent`.
    /// Any other state (`OwnedByOther`, `Ambiguous`, `Unowned`,
    /// missing/duplicate epic, lookup failure) yields `Err((code, message))`
    /// for the caller to wrap into a `JsonRpcResponse` with its own request id.
    ///
    /// Mirrors the gating shape used at the top of `handle_resume_plan`.
    /// Unlike `handle_resume_plan`, we do NOT auto-claim on `Unowned` here:
    /// these endpoints are mid-lifecycle operations, not entry points, and
    /// auto-claiming would mask bugs where a plan reaches review/merge with
    /// no recorded owner.
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

    async fn preview_task_base_impl(
        &self,
        input: crate::tool_schemas::PreviewTaskBaseInput,
    ) -> anyhow::Result<crate::tool_schemas::PreviewTaskBaseOutput> {
        let repo_root = self
            .repo_root
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Repository root not configured"))?;
        let plan_arc = self
            .load_or_project_plan(&input.plan_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        crate::plan::preview::preview_overlay(&plan_arc, &input.plan_id, &input.task_id, &repo_root)
            .await
    }

    async fn run_clobber_detector_for_review(
        &self,
        plan_arc: &Arc<tokio::sync::Mutex<crate::plan::PlanState>>,
        task_id: &str,
    ) -> Result<ClobberReviewReport, String> {
        let Some(repo_root) = self.repo_root.as_deref() else {
            return Ok(ClobberReviewReport::default());
        };

        let (issue_id, worker_branch, prior_candidates) = {
            let state = plan_arc.lock().await;
            let Some(current) = state
                .tasks
                .iter()
                .find(|entry| entry.spec.task_id == task_id)
            else {
                return Ok(ClobberReviewReport::default());
            };
            let Some(worker_branch) = current
                .worker_branch
                .clone()
                .filter(|branch| !branch.is_empty())
            else {
                return Ok(ClobberReviewReport::default());
            };
            let prior_candidates = state
                .tasks
                .iter()
                .filter(|entry| entry.spec.task_id != task_id)
                .filter(|entry| {
                    matches!(entry.status, crate::plan::PlanTaskStatus::Approved { .. })
                })
                .filter_map(|entry| {
                    let branch_name = entry.worker_branch.clone()?;
                    Some((entry.spec.task_id.clone(), branch_name))
                })
                .collect::<Vec<_>>();
            (
                current.spec.issue_id.clone(),
                worker_branch,
                prior_candidates,
            )
        };

        if prior_candidates.is_empty() {
            return Ok(ClobberReviewReport::default());
        }

        let mut warnings = Vec::new();
        let mut priors = Vec::with_capacity(prior_candidates.len());
        for (prior_task_id, branch_name) in prior_candidates {
            let tip_oid = match run_git_capture(
                repo_root,
                None,
                &["rev-parse", branch_name.as_str()],
            )
            .await
            {
                Ok(oid) => oid,
                Err(error) => {
                    tracing::warn!(
                        task_id = %prior_task_id,
                        branch = %branch_name,
                        "review_task clobber detector skipped prior: {error}"
                    );
                    warnings.push(format!(
                        "clobber detector skipped prior task '{prior_task_id}': {error}"
                    ));
                    continue;
                }
            };
            priors.push(crate::plan::clobber_detector::PriorTip {
                task_id: prior_task_id,
                branch_name,
                tip_oid,
            });
        }

        if priors.is_empty() {
            return Ok(ClobberReviewReport {
                signals: Vec::new(),
                warnings,
            });
        }

        let report = crate::plan::clobber_detector::run(repo_root, &worker_branch, &priors);
        if report.signals.is_empty() {
            return Ok(ClobberReviewReport {
                signals: Vec::new(),
                warnings,
            });
        }

        if let (Some(pm), Some(issue_id)) = (self.pm_service.as_deref(), issue_id.as_deref()) {
            if let Err(error) = require_feature(
                FeatureKey::PM_PRO_BEADS_ADVANCED,
                self.feature_gate.as_ref(),
            ) {
                let message = feature_error_message(error);
                tracing::warn!(
                    issue_id = %issue_id,
                    "review_task clobber detector signal persistence skipped: {message}"
                );
                warnings.push(format!(
                    "clobber detector could not write signal comments for issue '{issue_id}': {message}"
                ));
                return Ok(ClobberReviewReport {
                    signals: report.signals,
                    warnings,
                });
            }
            if let Some(advanced) = pm.advanced() {
                for signal in &report.signals {
                    if let Err(error) = advanced
                        .add_comment(issue_id, &crate::plan::signals::encode_comment(signal))
                        .await
                    {
                        tracing::warn!(
                            issue_id = %issue_id,
                            signal_id = %signal.signal_id(),
                            "review_task clobber detector signal comment failed: {error}"
                        );
                        warnings.push(format!(
                            "clobber detector failed to write signal comment for issue '{issue_id}': {error}"
                        ));
                    }
                }

                let mut add_labels = report
                    .signals
                    .iter()
                    .map(|signal| crate::plan::labels::signal_kind(signal.kind_label()))
                    .collect::<Vec<_>>();
                add_labels.sort();
                add_labels.dedup();
                if let Err(error) = pm
                    .update_issue(
                        issue_id,
                        IssueUpdate {
                            add_labels,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    tracing::warn!(
                        issue_id = %issue_id,
                        "review_task clobber detector signal label failed: {error}"
                    );
                    warnings.push(format!(
                        "clobber detector failed to add signal label for issue '{issue_id}': {error}"
                    ));
                }
            } else {
                warnings.push(format!(
                    "clobber detector could not write signal comments for issue '{issue_id}': beads advanced API unavailable"
                ));
            }
        }

        Ok(ClobberReviewReport {
            signals: report.signals,
            warnings,
        })
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

        // PR2 cache token: the monotonic sequence is the ordinal of audit
        // sentinel comments on the epic issue after projector ordering.
        // PR3 advances this epic sequence for every durable task-level plan
        // write; once landed, `versioned_cache_serve` can be flipped on.
        let audit_seq = crate::plan::projector::sort_projection_comments(comments)
            .into_iter()
            .filter(|comment| {
                comment
                    .body
                    .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX)
            })
            .count() as u64;
        Ok(BeadsVersion::AuditSeq(audit_seq))
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

    async fn recover_persisted_plans(&self, pm: Arc<spur_pm::PmService>) -> anyhow::Result<()> {
        let brain_session_id = self.brain_session_id_ready().await.clone();
        #[cfg(any(test, feature = "test-support"))]
        pause_startup_recovery_if_probed().await;
        let epics = pm
            .list_issues(spur_pm::IssueFilter {
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                limit: Some(1_000),
                ..Default::default()
            })
            .await?;

        for plan_id in discover_plan_ids_owned_by(&epics, brain_session_id.as_session_id()) {
            let projected = crate::plan::projector::project_plan_from_beads(
                pm.as_ref(),
                &plan_id,
                self.feature_gate.as_ref(),
            )
            .await?;
            for task in &projected.tasks {
                if let Some(issue_id) = &task.spec.issue_id {
                    compensate_mutation_orphans(
                        Arc::clone(&pm),
                        Arc::clone(&self.feature_gate),
                        issue_id,
                    )
                    .await?;
                    let _ = resolve_dispatch_orphan(
                        Arc::clone(&pm),
                        Arc::clone(&self.feature_gate),
                        issue_id,
                    )
                    .await?;
                }
            }
            let refreshed = crate::plan::projector::project_plan_from_beads(
                pm.as_ref(),
                &plan_id,
                self.feature_gate.as_ref(),
            )
            .await?;
            self.install_projected_plan(refreshed, true).await;
        }

        Ok(())
    }

    async fn sweep_stale_pending_plans_on_startup(
        &self,
        pm: Arc<spur_pm::PmService>,
    ) -> anyhow::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        pause_startup_recovery_if_probed().await;
        let pending_epics = pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                limit: Some(1_000),
                ..Default::default()
            })
            .await?;
        if pending_epics.is_empty() {
            return Ok(());
        }

        let now = chrono::Utc::now();
        let grace = chrono::Duration::from_std(self.plan_pending_grace)
            .unwrap_or_else(|_| chrono::Duration::hours(1));
        for summary in pending_epics {
            let epic = pm.get_issue(&summary.id).await?;
            let age = now.signed_duration_since(epic.created_at);
            if age < grace {
                continue;
            }

            let age_secs = age.num_seconds();
            let plan_id = epic
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
                .map(str::to_string);
            let Some(plan_id_value) = plan_id.as_deref() else {
                self.emit_plan_pending_sweep_event(
                    None,
                    &epic.id,
                    "skipped",
                    0,
                    age_secs,
                    "pending epic has no spur:plan-id label",
                );
                continue;
            };

            let children = self
                .list_plan_task_issues_for_pending_sweep(pm.as_ref(), plan_id_value)
                .await?;
            let mut skip_reason: Option<String> = None;
            for child in &children {
                match self
                    .pending_sweep_allows_child_status(pm.as_ref(), child)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        skip_reason = Some(format!(
                            "child '{}' is not open or previously quarantined",
                            child.id
                        ));
                        break;
                    }
                    Err(err) => {
                        skip_reason = Some(format!(
                            "comment lookup failed for child '{}': {err}",
                            child.id
                        ));
                        break;
                    }
                }
            }
            if let Some(reason) = skip_reason {
                self.emit_plan_pending_sweep_event(
                    plan_id.clone(),
                    &epic.id,
                    "skipped",
                    children.len() as u32,
                    age_secs,
                    &reason,
                );
                continue;
            }

            let comment = format!(
                "{PLAN_PENDING_SWEEP_COMMENT_PREFIX} `{}` (epic `{}`): graph stayed `{}` for {}s without flipping to `{}`. Children quarantined: {}.",
                plan_id_value,
                epic.id,
                crate::plan::labels::PLAN_PENDING,
                age_secs,
                crate::plan::labels::PLAN_COMPLETE,
                children.len()
            );
            let terminal_status = pm.closed_status().to_string();
            for child in &children {
                if child.status != "open" {
                    continue;
                }
                pm.update_issue(
                    &child.id,
                    IssueUpdate {
                        status: Some(terminal_status.clone()),
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to quarantine stale pending-plan child '{}'",
                        child.id
                    )
                })?;
            }
            pm.update_issue(
                &epic.id,
                IssueUpdate {
                    status: Some(terminal_status),
                    comment: Some(comment),
                    remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                    ..Default::default()
                },
            )
            .await
            .with_context(|| {
                format!("failed to quarantine stale pending-plan epic '{}'", epic.id)
            })?;

            self.emit_plan_pending_sweep_event(
                plan_id,
                &epic.id,
                "quarantined",
                children.len() as u32,
                age_secs,
                "stale pending plan exceeded grace",
            );
        }

        Ok(())
    }

    async fn pending_sweep_allows_child_status(
        &self,
        pm: &spur_pm::PmService,
        child: &spur_pm::Issue,
    ) -> anyhow::Result<bool> {
        if child.status == "open" {
            return Ok(true);
        }
        self.issue_has_plan_pending_sweep_comment(pm, &child.id)
            .await
    }

    async fn issue_has_plan_pending_sweep_comment(
        &self,
        pm: &spur_pm::PmService,
        issue_id: &str,
    ) -> anyhow::Result<bool> {
        if require_feature(
            FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .is_err()
        {
            return Ok(false);
        }
        let Some(advanced) = pm.advanced() else {
            return Ok(false);
        };
        let comments = advanced.list_comments(issue_id).await?;
        Ok(comments
            .iter()
            .any(|comment| comment.body.starts_with(PLAN_PENDING_SWEEP_COMMENT_PREFIX)))
    }

    async fn list_plan_task_issues_for_pending_sweep(
        &self,
        pm: &spur_pm::PmService,
        plan_id: &str,
    ) -> anyhow::Result<Vec<spur_pm::Issue>> {
        let summaries = pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("task".to_string()),
                include_closed: true,
                limit: Some(1_000),
                ..Default::default()
            })
            .await?;

        let mut issues = Vec::with_capacity(summaries.len());
        for summary in summaries {
            issues.push(pm.get_issue(&summary.id).await?);
        }
        Ok(issues)
    }

    fn emit_plan_pending_sweep_event(
        &self,
        plan_id: Option<String>,
        epic_id: &str,
        action: &str,
        child_count: u32,
        age_secs: i64,
        reason: &str,
    ) {
        tracing::warn!(
            target: "spur.plan_pending_sweep",
            plan_id = plan_id.as_deref().unwrap_or(""),
            %epic_id,
            %action,
            child_count,
            age_secs,
            %reason,
            "startup pending-plan sweep action"
        );
        if let Some(sink) = self.event_sink.as_deref() {
            sink.emit(spur_acp::SpurEventBody::PlanPendingSweep {
                plan_id,
                epic_id: epic_id.to_string(),
                action: action.to_string(),
                child_count,
                age_secs,
                reason: reason.to_string(),
            });
        }
    }

    async fn reclaim_persisted_plans_on_startup(
        &self,
        pm: Arc<spur_pm::PmService>,
    ) -> anyhow::Result<()> {
        debug!("startup recovery maintenance started");

        debug!("startup pending-plan sweep started");
        match self
            .sweep_stale_pending_plans_on_startup(Arc::clone(&pm))
            .await
        {
            Ok(()) => {
                debug!("startup pending-plan sweep finished");
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "startup pending-plan sweep failed; continuing startup recovery"
                );
            }
        }

        debug!("startup rev1 metadata check started");
        let has_rev1_metadata = match any_open_epic_lacks_rev1_metadata(
            pm.as_ref(),
            self.feature_gate.as_ref(),
        )
        .await
        {
            Ok(lacks_rev1_metadata) => {
                let has_rev1_metadata = !lacks_rev1_metadata;
                debug!(
                    has_rev1_metadata,
                    legacy_reclaim_needed = legacy_reclaim_needed(has_rev1_metadata),
                    "startup rev1 metadata check finished"
                );
                has_rev1_metadata
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "startup rev1 metadata check failed; skipping legacy persisted-plan recovery"
                );
                debug!("startup recovery maintenance finished");
                return Ok(());
            }
        };

        if legacy_reclaim_needed(has_rev1_metadata) {
            debug!("legacy persisted-plan startup recovery started");
            match self.recover_persisted_plans(pm).await {
                Ok(()) => {
                    debug!("legacy persisted-plan startup recovery finished");
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "legacy persisted-plan startup recovery failed"
                    );
                }
            }
        } else {
            debug!("legacy persisted-plan startup recovery skipped");
        }

        debug!("startup recovery maintenance finished");
        Ok(())
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
mod build_worker_info_tests {
    use super::build_worker_info;
    use spur_acp::config::AgentConfig;

    fn minimal_agent(name: &str) -> AgentConfig {
        let toml = format!(
            r#"name = "{}"
command = "x"
transport = "acp""#,
            name
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn build_worker_info_populates_all_fields() {
        let mut cfg = minimal_agent("claude-code-acp");
        spur_acp::agents::defaults::apply_builtin_defaults(&mut cfg);
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "claude-code-acp");
        assert!(info.description.is_some());
        assert!(info.tier.is_some());
        assert!(!info.good_for.is_empty());
        assert!(info.output_shape.is_some());
    }

    #[test]
    fn build_worker_info_handles_empty_descriptor() {
        let cfg = minimal_agent("unknown-agent");
        // without apply_builtin_defaults, all fields stay empty
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "unknown-agent");
        assert!(info.description.is_none());
        assert!(info.good_for.is_empty());
    }
}

#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
mod clobber_review_tests {
    use std::sync::Arc;

    use spur_acp::{BrainSessionId, SessionId};
    use tempfile::TempDir;

    fn no_op_ctx() -> super::DetachedContinuationCtx {
        super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
            .await
            .expect("git init");
        super::run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
            .await
            .expect("git config user.email");
        super::run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
            .await
            .expect("git config user.name");
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        super::run_git_capture(dir.path(), None, &["add", "README.md"])
            .await
            .expect("git add seed");
        super::run_git_capture(dir.path(), None, &["commit", "-q", "-m", "seed"])
            .await
            .expect("git commit seed");
        dir
    }

    async fn commit_worker_file(
        repo: &std::path::Path,
        branch: &str,
        path: &str,
        content: String,
    ) -> String {
        super::run_git_capture(repo, None, &["checkout", "-q", "-B", branch, "main"])
            .await
            .expect("checkout worker branch");
        std::fs::write(repo.join(path), content).expect("write worker file");
        super::run_git_capture(repo, None, &["add", path])
            .await
            .expect("git add worker file");
        super::run_git_capture(
            repo,
            None,
            &["commit", "-q", "-m", &format!("write {path}")],
        )
        .await
        .expect("git commit worker file");
        super::run_git_capture(repo, None, &["rev-parse", "HEAD"])
            .await
            .expect("git rev-parse worker tip")
    }

    fn task_entry(
        task_id: &str,
        status: crate::plan::PlanTaskStatus,
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) -> crate::plan::PlanTaskEntry {
        crate::plan::PlanTaskEntry {
            spec: crate::plan::PlanTask {
                task_id: task_id.to_string(),
                agent: "codex".to_string(),
                task: format!("task {task_id}"),
                depends_on: Vec::new(),
                issue_id: None,
                context_files: Vec::new(),
            },
            status,
            result: None,
            worker_branch: Some(worker_branch.to_string()),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: Some(format!("del-{task_id}")),
            dispatched_base_oid: Some(dispatched_base_oid.to_string()),
        }
    }

    #[tokio::test]
    async fn clobber_detector_for_review_uses_approved_branch_tip_not_dispatched_base_oid() {
        let dir = init_repo().await;
        let base_oid = super::run_git_capture(dir.path(), None, &["rev-parse", "main"])
            .await
            .expect("git rev-parse main");
        let worker_a_tip = commit_worker_file(
            dir.path(),
            "spur/test-clobber-worker-a",
            "foo.rs",
            "A".repeat(200),
        )
        .await;
        let worker_b_tip = commit_worker_file(
            dir.path(),
            "spur/test-clobber-worker-b",
            "foo.rs",
            "B".repeat(200),
        )
        .await;

        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.set_repo_root(dir.path().to_path_buf());

        let plan_arc = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-clobber".to_string(),
            tasks: vec![
                task_entry(
                    "T1",
                    crate::plan::PlanTaskStatus::Approved {
                        summary: Some("approved".to_string()),
                    },
                    "spur/test-clobber-worker-a",
                    &base_oid,
                ),
                task_entry(
                    "T2",
                    crate::plan::PlanTaskStatus::AwaitingReview {
                        summary: Some("awaiting review".to_string()),
                    },
                    "spur/test-clobber-worker-b",
                    &base_oid,
                ),
            ],
            brain_session_id: session_id,
            base_snapshot_branch: Some("main".to_string()),
            base_snapshot_oid: Some(base_oid.clone()),
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));

        let report = server
            .run_clobber_detector_for_review(&plan_arc, "T2")
            .await
            .expect("clobber detector review");

        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.signals.len(), 1, "{:?}", report.signals);
        match &report.signals[0] {
            crate::plan::signals::WorkerSignal::PotentialClobber {
                conflicting_task_id,
                file,
                upstream_tip,
                worker_tip,
                ..
            } => {
                assert_eq!(conflicting_task_id, "T1");
                assert_eq!(file, "foo.rs");
                assert_eq!(upstream_tip, &worker_a_tip);
                assert_eq!(worker_tip, &worker_b_tip);
                assert_ne!(
                    upstream_tip, &base_oid,
                    "prior tip must be the approved branch tip, not dispatched_base_oid"
                );
            }
            signal => panic!("expected PotentialClobber signal, got {signal:?}"),
        }
    }
}

#[cfg(test)]
#[cfg(test)]
fn attach_beads_workspace(repo: &std::path::Path, w: &spur_pm::test_workspace::TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    // Copy db + WAL + SHM (beads_rust uses WAL mode and skips checkpoint on
    // Drop; bare `fs::copy(beads.db)` loses every uncheckpointed write).
    w.copy_db_to(&beads_dir);
}

#[cfg(test)]
async fn init_beads_pm(
    repo: &std::path::Path,
) -> (
    spur_pm::test_workspace::TestBeadsWorkspace,
    std::sync::Arc<spur_pm::PmService>,
) {
    let w = spur_pm::test_workspace::TestBeadsWorkspace::init();
    attach_beads_workspace(repo, &w);

    let pm = std::sync::Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    (w, pm)
}

#[cfg(test)]
mod recover_orphaned_dispatch_tests {
    use super::{run_git_capture, DetachedContinuationCtx, McpCallbackServer};
    use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState};
    use crate::plan::PlanTask;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
            .await
            .expect("git init");
        run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
            .await
            .expect("git config user.email");
        run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
            .await
            .expect("git config user.name");
        dir
    }

    async fn commit_file(repo: &std::path::Path, path: &str, body: &str, message: &str) {
        std::fs::write(repo.join(path), body).expect("write file");
        run_git_capture(repo, None, &["add", path])
            .await
            .expect("git add");
        run_git_capture(repo, None, &["commit", "-q", "-m", message])
            .await
            .expect("git commit");
    }

    fn no_op_continuation_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_cont, _worker_session| Box::pin(async {})),
        }
    }

    fn response_text(response: &super::JsonRpcResponse) -> &str {
        response.result.as_ref().expect("success result")["content"][0]["text"]
            .as_str()
            .expect("text content")
    }

    struct RecoveryFixture {
        _beads: spur_pm::test_workspace::TestBeadsWorkspace,
        pm: Arc<spur_pm::PmService>,
        feature_gate: Arc<spur_license::FeatureGate>,
        task_issue_id: String,
    }

    async fn setup_recovery_task(
        repo: &std::path::Path,
        plan_id: &str,
        delegation_id: &str,
    ) -> RecoveryFixture {
        let (beads, pm) = super::init_beads_pm(repo).await;
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            "Recover orphan",
            None,
            &[PlanTask {
                task_id: "task-a".into(),
                agent: "codex".into(),
                task: "Recover this orphan".into(),
                depends_on: Vec::new(),
                issue_id: None,
                context_files: Vec::new(),
            }],
        )
        .await
        .expect("build epic subgraph");
        let task_issue_id = subgraph
            .task_map
            .get("task-a")
            .cloned()
            .expect("task issue id");
        crate::plan::persist_dispatch_intent(
            pm.as_ref(),
            &task_issue_id,
            feature_gate.as_ref(),
            plan_id,
            delegation_id,
            "codex",
            1,
            std::time::Duration::from_secs(600),
        )
        .await
        .expect("persist dispatch intent");

        RecoveryFixture {
            _beads: beads,
            pm,
            feature_gate,
            task_issue_id,
        }
    }

    fn recovery_server(
        repo: &std::path::Path,
        pm: Arc<spur_pm::PmService>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> McpCallbackServer {
        let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session),
            Some(pm),
            None,
            no_op_continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        server.set_repo_root(repo.to_path_buf());
        server
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_promotes_dispatched_task_to_awaiting_review() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/v2/codex/brain/worker";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
        run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
            .await
            .expect("checkout main");

        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let feature_gate = super::pro_feature_gate();
        let plan_id = "recover-orphan";
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            "Recover orphan",
            None,
            &[PlanTask {
                task_id: "task-a".into(),
                agent: "codex".into(),
                task: "Recover this orphan".into(),
                depends_on: Vec::new(),
                issue_id: None,
                context_files: Vec::new(),
            }],
        )
        .await
        .expect("build epic subgraph");
        let task_issue_id = subgraph
            .task_map
            .get("task-a")
            .cloned()
            .expect("task issue id");
        crate::plan::persist_dispatch_intent(
            pm.as_ref(),
            &task_issue_id,
            feature_gate.as_ref(),
            plan_id,
            "del-A",
            "codex",
            1,
            std::time::Duration::from_secs(600),
        )
        .await
        .expect("persist dispatch intent");

        let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session),
            Some(Arc::clone(&pm)),
            None,
            no_op_continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(dir.path().to_path_buf());

        let response = server
            .handle_tool_call(
                Value::Null,
                json!({
                    "name": "recover_orphaned_dispatch",
                    "arguments": {
                        "issue_id": task_issue_id.clone(),
                        "worker_branch": worker_branch,
                        "dispatched_base_oid": base_oid.clone(),
                    }
                }),
            )
            .await;

        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        assert!(
            response_text(&response).contains("Task promoted to AwaitingReview"),
            "unexpected response: {}",
            response_text(&response)
        );

        let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
        assert!(
            crate::plan::projector::has_ready_for_review_label_compat(&issue.labels),
            "recovered task must have ready-for-review label: {:?}",
            issue.labels
        );
        assert!(
            !issue
                .labels
                .contains(&crate::plan::labels::delegation_id("del-A")),
            "recovered task must clear dispatch label: {:?}",
            issue.labels
        );

        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("test feature gate should allow beads advanced");
        let adv = pm.advanced().expect("advanced beads backend");
        let audits = crate::plan::projector::collect_sorted_audits_for_issue(
            &task_issue_id,
            adv.list_comments(&task_issue_id)
                .await
                .expect("list comments"),
        );
        let completion = audits.iter().find_map(|audit| match audit {
            AuditSentinelKind::Completion {
                delegation_id,
                completion_state,
                worker_branch: found_branch,
                dispatched_base_oid,
                ..
            } if delegation_id == "del-A" => Some((
                *completion_state,
                found_branch.as_deref(),
                dispatched_base_oid.as_deref(),
            )),
            _ => None,
        });
        assert_eq!(
            completion,
            Some((
                CompletionState::AwaitingReview,
                Some(worker_branch),
                Some(base_oid.as_str())
            ))
        );
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_prefers_dispatched_base_oid_label() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/recover-from-label";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
        run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
            .await
            .expect("checkout main");
        commit_file(dir.path(), "wrong-base.txt", "wrong\n", "wrong base").await;
        let wrong_base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("wrong base oid");

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        fixture
            .pm
            .update_issue(
                &fixture.task_issue_id,
                spur_pm::IssueUpdate {
                    add_labels: vec![crate::plan::labels::dispatched_base_oid(&base_oid)],
                    ..Default::default()
                },
            )
            .await
            .expect("persist dispatched base oid label");
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let message = server
            .recover_orphaned_dispatch_with_branch(
                &fixture.task_issue_id,
                worker_branch,
                &wrong_base_oid,
            )
            .await
            .expect("label-backed recovery should succeed");
        assert!(
            message.contains("Task promoted to AwaitingReview"),
            "unexpected response: {message}"
        );

        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            super::pro_feature_gate().as_ref(),
        )
        .expect("fixture enables advanced beads");
        let adv = fixture.pm.advanced().expect("advanced beads backend");
        let audits = crate::plan::projector::collect_sorted_audits_for_issue(
            &fixture.task_issue_id,
            adv.list_comments(&fixture.task_issue_id)
                .await
                .expect("list comments"),
        );
        let recovered_base = audits.iter().find_map(|audit| match audit {
            AuditSentinelKind::Completion {
                delegation_id,
                dispatched_base_oid,
                ..
            } if delegation_id == "del-A" => dispatched_base_oid.as_deref(),
            _ => None,
        });
        assert_eq!(recovered_base, Some(base_oid.as_str()));
    }

    #[tokio::test]
    #[ignore = "pinned residual; requires deterministic-recovery follow-up"]
    async fn recover_orphaned_dispatch_with_split_dispatched_base_oid_labels() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let worker_branch = "spur/worker/split-dispatched-base-labels";
        run_git_capture(dir.path(), None, &["branch", worker_branch])
            .await
            .expect("create worker branch");

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        fixture
            .pm
            .update_issue(
                &fixture.task_issue_id,
                spur_pm::IssueUpdate {
                    add_labels: vec![
                        crate::plan::labels::dispatched_base_oid("aaa1"),
                        crate::plan::labels::dispatched_base_oid("bbb2"),
                    ],
                    ..Default::default()
                },
            )
            .await
            .expect("persist split dispatched base oid labels");
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(
                &fixture.task_issue_id,
                worker_branch,
                "fallback-base",
            )
            .await
            .expect_err("current split-label behavior selects a non-git OID and fails validation");
        assert!(
            err.contains("base=aaa1"),
            "current split-label recovery should select the first label; got: {err}"
        );
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_more_than_one_commit() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/two-commits";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker-a.txt", "a\n", "worker change a").await;
        commit_file(dir.path(), "worker-b.txt", "b\n", "worker change b").await;

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
            .await
            .expect_err("two worker commits must be rejected");
        assert!(
            err.contains("2 commits") || err.contains("expected exactly 1"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_zero_commits() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/zero-commits";
        run_git_capture(dir.path(), None, &["branch", worker_branch, &base_oid])
            .await
            .expect("create worker branch");

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
            .await
            .expect_err("zero worker commits must be rejected");
        assert!(err.contains("0 commits"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_already_completed_delegation() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/already-completed";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            fixture.feature_gate.as_ref(),
        )
        .expect("test feature gate should allow beads advanced");
        let adv = fixture.pm.advanced().expect("advanced beads backend");
        adv.add_comment(
            &fixture.task_issue_id,
            &crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some(worker_branch.into()),
                result_summary: Some("already done".into()),
                artifact_uri: None,
                dispatched_base_oid: Some(base_oid.clone()),
            }),
        )
        .await
        .expect("completion audit");
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
            .await
            .expect_err("already completed delegation must be rejected");
        assert!(
            err.contains("already has a completion audit"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_missing_branch() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(
                &fixture.task_issue_id,
                "spur/worker/does-not-exist",
                &base_oid,
            )
            .await
            .expect_err("missing worker branch must be rejected");
        assert!(err.contains("not found"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_missing_plan_id() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/missing-plan-id";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;

        let fixture =
            setup_recovery_task(dir.path(), "recover-orphan-missing-plan-id", "del-A").await;
        fixture
            .pm
            .update_issue(
                &fixture.task_issue_id,
                spur_pm::IssueUpdate {
                    remove_labels: vec![crate::plan::labels::plan_id(
                        "recover-orphan-missing-plan-id",
                    )],
                    ..Default::default()
                },
            )
            .await
            .expect("remove plan id label");
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
            .await
            .expect_err("missing plan-id label must be rejected");
        assert!(
            err.contains("missing spur:plan-id"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn recover_orphaned_dispatch_rejects_non_ancestor_base() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let original_base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("base oid");
        let worker_branch = "spur/worker/diverged-base";
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "-b", worker_branch, &original_base_oid],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
        run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
            .await
            .expect("checkout main");
        commit_file(dir.path(), "main.txt", "main\n", "main moved").await;
        let non_ancestor_base = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
            .await
            .expect("non-ancestor base oid");

        let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
        let server = recovery_server(
            dir.path(),
            Arc::clone(&fixture.pm),
            Arc::clone(&fixture.feature_gate),
        );

        let err = server
            .recover_orphaned_dispatch_with_branch(
                &fixture.task_issue_id,
                worker_branch,
                &non_ancestor_base,
            )
            .await
            .expect_err("non-ancestor base must be rejected");
        assert!(
            err.contains("not an ancestor") || err.contains("G-Strict validation failed"),
            "unexpected error: {err}"
        );
    }
}

#[cfg(test)]
mod merge_plan_tests {
    use super::{integrate_plan_branches, resolve_plan_base, run_git_capture, JsonRpcResponse};
    use crate::plan::audit_sentinel::{encode_comment, AuditSentinelKind, CompletionState};
    use crate::plan::{PlanMergeState, PlanTask};
    use serde_json::{json, Value};
    use spur_pm::test_workspace::TestBeadsWorkspace;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git_capture(dir.path(), None, &["init", "-q"])
            .await
            .expect("git init");
        run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
            .await
            .expect("git config user.email");
        run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
            .await
            .expect("git config user.name");
        dir
    }

    async fn commit_file(repo: &std::path::Path, path: &str, body: &str, message: &str) {
        std::fs::write(repo.join(path), body).expect("write file");
        run_git_capture(repo, None, &["add", path])
            .await
            .expect("git add");
        run_git_capture(repo, None, &["commit", "-q", "-m", message])
            .await
            .expect("git commit");
    }

    struct PersistedMergeFixture {
        _dir: TempDir,
        _beads: TestBeadsWorkspace,
        pm: Arc<spur_pm::PmService>,
        server: super::McpCallbackServer,
        plan_id: String,
        epic_id: String,
    }

    async fn setup_persisted_merge_ready_plan(
        plan_id: &str,
        clear_cache: bool,
    ) -> PersistedMergeFixture {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let (beads, pm) = super::init_beads_pm(dir.path()).await;

        run_git_capture(
            dir.path(),
            None,
            &["branch", "spur/brain-snapshot-test", "HEAD"],
        )
        .await
        .expect("snapshot branch");
        let base_snapshot_oid = run_git_capture(
            dir.path(),
            None,
            &["rev-parse", "--verify", "spur/brain-snapshot-test"],
        )
        .await
        .expect("snapshot oid");

        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker branch");
        commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot branch");

        let tasks = vec![PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Integrate worker branch".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner("brain")],
                ..Default::default()
            },
        )
        .await
        .expect("stamp plan_owner label on fixture epic");
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced beads backend");
        crate::emit_plan_submit_audit(
            adv,
            plan_id,
            &subgraph,
            crate::PlanSubmitAuditContext {
                base_snapshot_branch: Some("spur/brain-snapshot-test"),
                base_snapshot_oid: Some(base_snapshot_oid.as_str()),
                execution_mode: Some("submit_plan"),
                brain_session_id: None,
                explicit_base: None,
            },
        )
        .await;

        let task_issue_id = subgraph
            .task_map
            .get("task-a")
            .cloned()
            .expect("task issue id");
        adv.add_comment(
            &task_issue_id,
            &encode_comment(&AuditSentinelKind::Completion {
                delegation_id: "del-1".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-a".into()),
                result_summary: Some("worker branch ready".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            }),
        )
        .await
        .expect("completion audit");
        adv.add_comment(
            &task_issue_id,
            &encode_comment(&AuditSentinelKind::Approval {
                delegation_id: "del-1".into(),
            }),
        )
        .await
        .expect("approval audit");
        pm.update_issue(
            &task_issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close task issue");

        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(dir.path().to_path_buf());

        let projected = crate::plan::projector::project_plan_from_beads(
            pm.as_ref(),
            plan_id,
            feature_gate.as_ref(),
        )
        .await
        .expect("project persisted plan");
        assert_eq!(
            crate::plan::build_plan_status(plan_id, &projected)["ready_to_merge"],
            Value::Bool(true)
        );
        server.install_projected_plan(projected, false).await;
        if clear_cache {
            server.active_plans.lock().await.remove(plan_id);
        }

        PersistedMergeFixture {
            _dir: dir,
            _beads: beads,
            pm,
            server,
            plan_id: plan_id.to_string(),
            epic_id: subgraph.epic_id,
        }
    }

    async fn setup_persisted_retried_plan(
        plan_id: &str,
        clear_cache: bool,
    ) -> PersistedMergeFixture {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let (beads, pm) = super::init_beads_pm(dir.path()).await;

        run_git_capture(
            dir.path(),
            None,
            &["branch", "spur/brain-snapshot-test", "HEAD"],
        )
        .await
        .expect("snapshot branch");
        let base_snapshot_oid = run_git_capture(
            dir.path(),
            None,
            &["rev-parse", "--verify", "spur/brain-snapshot-test"],
        )
        .await
        .expect("snapshot oid");

        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a1",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-a1");
        commit_file(
            dir.path(),
            "worker-1.txt",
            "attempt-1\n",
            "worker attempt 1",
        )
        .await;

        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot branch");
        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a2",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-a2");
        commit_file(
            dir.path(),
            "worker-2.txt",
            "attempt-2\n",
            "worker attempt 2",
        )
        .await;

        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot branch");

        let tasks = vec![PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Integrate worker branch".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner("brain")],
                ..Default::default()
            },
        )
        .await
        .expect("stamp plan_owner label on fixture epic");
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced beads backend");
        crate::emit_plan_submit_audit(
            adv,
            plan_id,
            &subgraph,
            crate::PlanSubmitAuditContext {
                base_snapshot_branch: Some("spur/brain-snapshot-test"),
                base_snapshot_oid: Some(base_snapshot_oid.as_str()),
                execution_mode: Some("submit_plan"),
                brain_session_id: None,
                explicit_base: None,
            },
        )
        .await;

        let task_issue_id = subgraph
            .task_map
            .get("task-a")
            .cloned()
            .expect("task issue id");
        for audit in [
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-1".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-a1".into()),
                result_summary: Some("attempt 1 summary".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::Rejection {
                delegation_id: "del-1".into(),
                feedback: "needs changes".into(),
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".into(),
                worker: "codex".into(),
                attempt: 2,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-2".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-a2".into()),
                result_summary: Some("attempt 2 summary".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "del-2".into(),
            },
        ] {
            adv.add_comment(&task_issue_id, &encode_comment(&audit))
                .await
                .expect("attempt audit");
        }
        pm.update_issue(
            &task_issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close task issue");

        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(dir.path().to_path_buf());

        let projected = crate::plan::projector::project_plan_from_beads(
            pm.as_ref(),
            plan_id,
            feature_gate.as_ref(),
        )
        .await
        .expect("project persisted plan");
        assert_eq!(
            crate::plan::build_plan_status(plan_id, &projected)["ready_to_merge"],
            Value::Bool(true)
        );
        server.install_projected_plan(projected, false).await;
        if clear_cache {
            server.active_plans.lock().await.remove(plan_id);
        }

        PersistedMergeFixture {
            _dir: dir,
            _beads: beads,
            pm,
            server,
            plan_id: plan_id.to_string(),
            epic_id: subgraph.epic_id,
        }
    }

    #[tokio::test]
    async fn reconstruct_historical_attempts_classifies_retry_requested_as_worker_failure_recovery()
    {
        let dir = init_repo().await;
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let feature_gate = super::pro_feature_gate();
        let task_issue_id = pm
            .create_issue(spur_pm::IssueCreate {
                title: "Retry reconstruction task".into(),
                description: Some("task body".into()),
                ..Default::default()
            })
            .await
            .expect("create task issue");
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced beads backend");

        for audit in [
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-1".into(),
                completion_state: CompletionState::Failed,
                superseded: false,
                worker_branch: Some("spur/worker-failed".into()),
                result_summary: Some("worker crashed".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::RetryRequested {
                delegation_id: "del-1".into(),
                attempt: 1,
                error: "worker crashed".into(),
                worker_branch: Some("spur/worker-failed".into()),
                amended_prompt_summary: None,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".into(),
                worker: "codex".into(),
                attempt: 2,
            },
        ] {
            adv.add_comment(&task_issue_id, &encode_comment(&audit))
                .await
                .expect("attempt audit");
        }

        let history = super::reconstruct_historical_attempts(
            pm.as_ref(),
            feature_gate.as_ref(),
            &task_issue_id,
            2,
        )
        .await
        .expect("reconstruct history");

        assert_eq!(history.len(), 1);
        let attempt = &history[0];
        assert_eq!(attempt.attempt, 1);
        assert_eq!(attempt.worker_branch.as_deref(), Some("spur/worker-failed"));
        assert_eq!(
            attempt.kind(),
            crate::plan::AttemptRecordKind::WorkerFailureRecovery
        );
    }

    async fn setup_cached_overlay_diff_plan(
        plan_id: &str,
        use_dispatched_base_oid: bool,
    ) -> PersistedMergeFixture {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        let (beads, pm) = super::init_beads_pm(dir.path()).await;

        run_git_capture(
            dir.path(),
            None,
            &["branch", "spur/brain-snapshot-test", "HEAD"],
        )
        .await
        .expect("snapshot branch");
        let base_snapshot_oid = run_git_capture(
            dir.path(),
            None,
            &["rev-parse", "--verify", "spur/brain-snapshot-test"],
        )
        .await
        .expect("snapshot oid");

        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-a");
        commit_file(dir.path(), "foo.rs", "fn foo() {}\n", "task a").await;

        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot");
        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-b",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-b");
        run_git_capture(dir.path(), None, &["cherry-pick", "spur/worker-a"])
            .await
            .expect("apply task-a overlay");
        let t2_dispatched_base_oid =
            run_git_capture(dir.path(), None, &["rev-parse", "--verify", "HEAD"])
                .await
                .expect("overlay base oid");
        commit_file(dir.path(), "bar.rs", "fn bar() {}\n", "task b").await;
        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot");

        let tasks = vec![
            PlanTask {
                task_id: "task-a".into(),
                agent: "codex".into(),
                task: "Create foo".into(),
                depends_on: Vec::new(),
                issue_id: None,
                context_files: Vec::new(),
            },
            PlanTask {
                task_id: "task-b".into(),
                agent: "codex".into(),
                task: "Create bar".into(),
                depends_on: vec!["task-a".into()],
                issue_id: None,
                context_files: Vec::new(),
            },
        ];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner("brain")],
                ..Default::default()
            },
        )
        .await
        .expect("stamp plan_owner label on fixture epic");

        let task_a_issue_id = subgraph
            .task_map
            .get("task-a")
            .cloned()
            .expect("task-a issue id");
        let task_b_issue_id = subgraph
            .task_map
            .get("task-b")
            .cloned()
            .expect("task-b issue id");
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced beads backend");
        crate::emit_plan_submit_audit(
            adv,
            plan_id,
            &subgraph,
            crate::PlanSubmitAuditContext {
                base_snapshot_branch: Some("spur/brain-snapshot-test"),
                base_snapshot_oid: Some(base_snapshot_oid.as_str()),
                execution_mode: Some("submit_plan"),
                brain_session_id: None,
                explicit_base: None,
            },
        )
        .await;
        for (issue_id, audit) in [
            (
                task_a_issue_id.as_str(),
                AuditSentinelKind::Completion {
                    delegation_id: "del-a".into(),
                    completion_state: CompletionState::AwaitingReview,
                    superseded: false,
                    worker_branch: Some("spur/worker-a".into()),
                    result_summary: Some("foo ready".into()),
                    artifact_uri: None,
                    dispatched_base_oid: Some(base_snapshot_oid.clone()),
                },
            ),
            (
                task_b_issue_id.as_str(),
                AuditSentinelKind::Completion {
                    delegation_id: "del-b".into(),
                    completion_state: CompletionState::AwaitingReview,
                    superseded: false,
                    worker_branch: Some("spur/worker-b".into()),
                    result_summary: Some("bar ready".into()),
                    artifact_uri: None,
                    dispatched_base_oid: use_dispatched_base_oid.then_some(t2_dispatched_base_oid),
                },
            ),
        ] {
            adv.add_comment(issue_id, &encode_comment(&audit))
                .await
                .expect("completion audit");
            let delegation_id = match audit {
                AuditSentinelKind::Completion { delegation_id, .. } => delegation_id,
                _ => unreachable!("test fixture only emits completions"),
            };
            adv.add_comment(
                issue_id,
                &encode_comment(&AuditSentinelKind::Approval { delegation_id }),
            )
            .await
            .expect("approval audit");
            pm.update_issue(
                issue_id,
                spur_pm::IssueUpdate {
                    status: Some(pm.closed_status().to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("close task issue");
        }

        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(dir.path().to_path_buf());
        let projected = crate::plan::projector::project_plan_from_beads(
            pm.as_ref(),
            plan_id,
            feature_gate.as_ref(),
        )
        .await
        .expect("project persisted plan");
        server.install_projected_plan(projected, false).await;

        PersistedMergeFixture {
            _dir: dir,
            _beads: beads,
            pm,
            server,
            plan_id: plan_id.to_string(),
            epic_id: subgraph.epic_id,
        }
    }

    fn decode_merge_status(response: super::JsonRpcResponse) -> Value {
        assert!(
            response.error.is_none(),
            "merge_plan should succeed: {:?}",
            response.error
        );

        let result = response.result.expect("merge_plan result");
        let text = result["content"][0]["text"]
            .as_str()
            .expect("merge_plan text response");
        serde_json::from_str(text).expect("merge_plan status JSON")
    }

    fn decode_task_diff_response(text: &str) -> Value {
        serde_json::from_str(text).expect("get_task_diff response JSON")
    }

    fn task_diff_text(response: JsonRpcResponse) -> String {
        let result = response
            .result
            .expect("get_task_diff JsonRpcResponse must be Ok");
        result["content"][0]["text"]
            .as_str()
            .expect("get_task_diff response must carry content[0].text")
            .to_string()
    }

    #[derive(Clone, Default)]
    struct CapturedWarnings {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl CapturedWarnings {
        fn contains(&self, needle: &str) -> bool {
            self.events
                .lock()
                .expect("warning capture lock")
                .iter()
                .any(|event| event.contains(needle))
        }
    }

    impl tracing::Subscriber for CapturedWarnings {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }

            struct Visitor {
                fields: String,
            }

            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if !self.fields.is_empty() {
                        self.fields.push(' ');
                    }
                    self.fields.push_str(field.name());
                    self.fields.push('=');
                    self.fields.push_str(&format!("{value:?}"));
                }
            }

            let mut visitor = Visitor {
                fields: String::new(),
            };
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("warning capture lock")
                .push(visitor.fields);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn resolve_plan_base_captures_oid() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;

        let repo_root = dir.path().to_path_buf();
        let snapshot = resolve_plan_base(Some(&repo_root), None)
            .await
            .expect("resolve_plan_base");

        let expected_oid = run_git_capture(
            dir.path(),
            None,
            &[
                "rev-parse",
                "--verify",
                snapshot.branch.as_deref().expect("snapshot branch"),
            ],
        )
        .await
        .expect("rev-parse snapshot branch");

        assert_eq!(snapshot.oid.as_deref(), Some(expected_oid.as_str()));
    }

    #[tokio::test]
    async fn integrate_plan_branches_succeeds_without_touching_active_checkout() {
        let dir = init_repo().await;
        commit_file(dir.path(), "base.txt", "base\n", "seed").await;
        run_git_capture(
            dir.path(),
            None,
            &["branch", "spur/brain-snapshot-test", "HEAD"],
        )
        .await
        .expect("snapshot branch");

        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-a");
        commit_file(dir.path(), "a.txt", "alpha\n", "worker a").await;

        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot");
        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-b",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-b");
        commit_file(dir.path(), "b.txt", "beta\n", "worker b").await;

        let outcome = integrate_plan_branches(
            dir.path(),
            "spur/brain-snapshot-test",
            "spur/plan-merge-test",
            &[
                ("task-a".into(), "spur/worker-a".into()),
                ("task-b".into(), "spur/worker-b".into()),
            ],
        )
        .await
        .expect("integration should succeed");

        match outcome {
            PlanMergeState::Succeeded {
                merge_branch,
                merged_task_ids,
            } => {
                assert_eq!(merge_branch, "spur/plan-merge-test");
                assert_eq!(merged_task_ids, vec!["task-a", "task-b"]);
            }
            other => panic!("expected successful merge state, got {other:?}"),
        }

        let a_contents = run_git_capture(dir.path(), None, &["show", "spur/plan-merge-test:a.txt"])
            .await
            .expect("show merged a.txt");
        let b_contents = run_git_capture(dir.path(), None, &["show", "spur/plan-merge-test:b.txt"])
            .await
            .expect("show merged b.txt");
        assert_eq!(a_contents, "alpha");
        assert_eq!(b_contents, "beta");
    }

    #[tokio::test]
    async fn integrate_plan_branches_reports_conflict_and_keeps_partial_branch() {
        let dir = init_repo().await;
        commit_file(dir.path(), "shared.txt", "base\n", "seed").await;
        run_git_capture(
            dir.path(),
            None,
            &["branch", "spur/brain-snapshot-test", "HEAD"],
        )
        .await
        .expect("snapshot branch");

        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-a",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-a");
        commit_file(dir.path(), "shared.txt", "worker-a\n", "worker a").await;

        run_git_capture(
            dir.path(),
            None,
            &["checkout", "-q", "spur/brain-snapshot-test"],
        )
        .await
        .expect("checkout snapshot");
        run_git_capture(
            dir.path(),
            None,
            &[
                "checkout",
                "-q",
                "-b",
                "spur/worker-b",
                "spur/brain-snapshot-test",
            ],
        )
        .await
        .expect("checkout worker-b");
        commit_file(dir.path(), "shared.txt", "worker-b\n", "worker b").await;

        let outcome = integrate_plan_branches(
            dir.path(),
            "spur/brain-snapshot-test",
            "spur/plan-merge-conflict",
            &[
                ("task-a".into(), "spur/worker-a".into()),
                ("task-b".into(), "spur/worker-b".into()),
            ],
        )
        .await
        .expect("integration should return conflict state");

        match outcome {
            PlanMergeState::Conflict {
                merge_branch,
                conflict_task_id,
                conflict_worker_branch,
                merged_task_ids,
                files,
            } => {
                assert_eq!(merge_branch, "spur/plan-merge-conflict");
                assert_eq!(conflict_task_id, "task-b");
                assert_eq!(conflict_worker_branch, "spur/worker-b");
                assert_eq!(merged_task_ids, vec!["task-a"]);
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "conflict files should mention shared.txt: {files:?}"
                );
            }
            other => panic!("expected conflict merge state, got {other:?}"),
        }

        let merged_contents = run_git_capture(
            dir.path(),
            None,
            &["show", "spur/plan-merge-conflict:shared.txt"],
        )
        .await
        .expect("show partial merge branch");
        assert_eq!(merged_contents, "worker-a");
    }

    #[tokio::test]
    async fn merge_plan_rehydrates_when_cache_missing() {
        let fixture = setup_persisted_merge_ready_plan("plan-merge-recover", true).await;

        let response = fixture
            .server
            .handle_merge_plan(Value::Null, json!({ "plan_id": fixture.plan_id }))
            .await;
        let status = decode_merge_status(response);
        assert_eq!(status["merge"]["status"], "succeeded");
        assert_eq!(status["ready_to_merge"], true);
        assert_eq!(status["merge"]["merged_task_ids"], json!(["task-a"]));
    }

    #[tokio::test]
    async fn merge_plan_clears_integration_pending_on_success() {
        let fixture = setup_persisted_merge_ready_plan("plan-merge-clear-label", true).await;
        fixture
            .pm
            .update_issue(
                &fixture.epic_id,
                spur_pm::IssueUpdate {
                    add_labels: vec![crate::plan::labels::INTEGRATION_PENDING.to_string()],
                    ..Default::default()
                },
            )
            .await
            .expect("add integration-pending label");

        let response = fixture
            .server
            .handle_merge_plan(Value::Null, json!({ "plan_id": fixture.plan_id }))
            .await;
        let status = decode_merge_status(response);
        assert_eq!(status["merge"]["status"], "succeeded");

        let epic = fixture
            .pm
            .get_issue(&fixture.epic_id)
            .await
            .expect("get epic");
        assert!(
            !epic
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::INTEGRATION_PENDING),
            "merge_plan should clear integration-pending: {:?}",
            epic.labels
        );
    }

    #[tokio::test]
    async fn get_task_diff_rehydrates_latest_attempt_when_cache_missing() {
        let fixture = setup_persisted_merge_ready_plan("plan-diff-recover", true).await;

        let raw = fixture
            .server
            .handle_get_task_diff(
                json!(1),
                json!({
                    "plan_id": fixture.plan_id,
                    "task_id": "task-a",
                }),
            )
            .await;
        let text = task_diff_text(raw);
        let response = decode_task_diff_response(&text);

        assert_eq!(response["worker_branch"], "spur/worker-a");
        assert_eq!(response["summary"], "worker branch ready");
        assert!(
            response["diff"]
                .as_str()
                .map(|diff| diff.contains("worker.txt"))
                .unwrap_or(false),
            "latest-attempt cache miss should rebuild full diff text: {response}"
        );
    }

    #[tokio::test]
    async fn get_task_diff_uses_dispatched_base_oid_when_present() {
        let fixture = setup_cached_overlay_diff_plan("plan-diff-overlay", true).await;

        let raw = fixture
            .server
            .handle_get_task_diff(
                json!(1),
                json!({
                    "plan_id": fixture.plan_id,
                    "task_id": "task-b",
                }),
            )
            .await;
        let text = task_diff_text(raw);
        let response = decode_task_diff_response(&text);
        let diff = response["diff"].as_str().expect("diff text");

        assert!(
            diff.contains("bar.rs"),
            "task-b diff should include its own change: {diff}"
        );
        assert!(
            !diff.contains("foo.rs"),
            "task-b diff must not include inherited task-a overlay: {diff}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_task_diff_warns_and_falls_back_for_legacy_task() {
        let fixture = setup_cached_overlay_diff_plan("plan-diff-legacy", false).await;
        let warnings = CapturedWarnings::default();
        let _guard = tracing::subscriber::set_default(warnings.clone());

        let raw = fixture
            .server
            .handle_get_task_diff(
                json!(1),
                json!({
                    "plan_id": fixture.plan_id,
                    "task_id": "task-b",
                }),
            )
            .await;
        let text = task_diff_text(raw);
        let response = decode_task_diff_response(&text);
        let diff = response["diff"].as_str().expect("diff text");

        assert!(
            diff.contains("foo.rs"),
            "legacy fallback should retain the base snapshot range: {diff}"
        );
        assert!(
            diff.contains("bar.rs"),
            "legacy fallback should include the worker change: {diff}"
        );
        assert!(
            warnings.contains("dispatched_base_oid"),
            "legacy fallback should emit a warning mentioning dispatched_base_oid"
        );
    }

    #[tokio::test]
    async fn get_task_diff_historical_attempts_remain_summary_only() {
        let fixture = setup_persisted_retried_plan("plan-diff-history", true).await;

        let raw = fixture
            .server
            .handle_get_task_diff(
                json!(1),
                json!({
                    "plan_id": fixture.plan_id,
                    "task_id": "task-a",
                    "attempt": 1,
                }),
            )
            .await;
        let text = task_diff_text(raw);
        let response = decode_task_diff_response(&text);

        assert_eq!(response["status"], "historical");
        assert_eq!(response["worker_branch"], "spur/worker-a1");
        assert_eq!(response["summary"], "attempt 1 summary");
        assert_eq!(response["feedback"], "needs changes");
        assert!(
            response.get("diff").is_none(),
            "historical responses must remain summary-only: {response}"
        );
        assert!(
            response["note"]
                .as_str()
                .map(|note| note.contains("Historical attempt"))
                .unwrap_or(false),
            "historical responses must explain the summary-only contract: {response}"
        );
    }

    #[tokio::test]
    async fn comment_lookup_returns_false_when_advanced_feature_unlicensed() {
        use spur_acp::{BrainSessionId, SessionId};

        let dir = TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;

        let session_id = BrainSessionId::new(SessionId("comment-lookup-non-pro".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            outcome_store,
            super::community_feature_gate(),
        );

        let issue_id = pm
            .create_issue(spur_pm::IssueCreate {
                title: "non-pro probe".into(),
                issue_type: Some("task".into()),
                ..Default::default()
            })
            .await
            .expect("create issue");

        pm.update_issue(
            &issue_id,
            spur_pm::IssueUpdate {
                comment: Some(format!(
                    "{} `non-pro` quarantine seed.",
                    super::PLAN_PENDING_SWEEP_COMMENT_PREFIX
                )),
                ..Default::default()
            },
        )
        .await
        .expect("seed prefix comment");

        let result = server
            .issue_has_plan_pending_sweep_comment(pm.as_ref(), &issue_id)
            .await
            .expect("non-pro lookup must not propagate an error");
        assert!(
            !result,
            "non-pro feature gate must yield Ok(false) so the sweep skips conservatively instead of aborting"
        );
    }
}
