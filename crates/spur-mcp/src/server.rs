use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

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
use crate::plan::proposers::{ScopeDriftSplitProposer, TrivialScorer};
use crate::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use crate::plan::signal_watcher::SignalWatcher;
use crate::tools::{self, DelegationChannel, DelegationRequest};

/// How long completed delegation results are retained before lazy eviction.
///
/// Phase 4: the `completed_delegations` map is preserved as a TTL-bounded
/// debug buffer (per the async-first design spec). After Part A removed
/// `delegate_async` / `wait_delegation`, no handler writes the map under
/// normal operation — `BlockTimeout` collectors skip it (INV-ASYNC-2).
/// The 60 s TTL is generous for any residual debug-injection use; the
/// map is allowed to stay permanently empty in production.
const COMPLETED_TTL: std::time::Duration = std::time::Duration::from_secs(60);
const DEFAULT_PLAN_PENDING_GRACE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const BRAIN_SESSION_BIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Prefix used to mark a comment as a startup-sweep quarantine audit.
///
/// **DO NOT RENAME WITHOUT MIGRATION.** This string is durable state: the
/// startup sweep retry path (`pending_sweep_allows_child_status`) treats the
/// presence of a comment with this prefix as proof that a child was
/// quarantined by a previous sweep run. Changing this constant will break
/// resumption of any sweep that was interrupted under the old value.
const PLAN_PENDING_SWEEP_COMMENT_PREFIX: &str = "SPUR startup sweep quarantined stale pending plan";
pub(crate) const ORPHAN_CLEAR_REASON_RESTART: &str = "restart-orphan-cleared";
/// Idle-session watchdog for the streamable-HTTP MCP transport.
///
/// rmcp's `SessionConfig::DEFAULT_KEEP_ALIVE` is 5 min, which is far too short
/// for brain↔spur sessions where a brain agent commonly idles between user
/// turns (lunch, overnight, parallel work in another window). When the watchdog
/// fires, the worker quits and rmcp's tower layer logs a cascading
/// `Failed to close session ... Session service terminated` ERROR. 4 hours
/// preserves cleanup of truly-orphaned sessions while accommodating realistic
/// idle gaps. Override via `SPUR_MCP_SESSION_KEEPALIVE_SECS` (env var, secs;
/// `0` disables the watchdog entirely).
const MCP_SESSION_KEEPALIVE_DEFAULT: std::time::Duration =
    std::time::Duration::from_secs(4 * 60 * 60);

fn mcp_session_keepalive() -> Option<std::time::Duration> {
    match std::env::var("SPUR_MCP_SESSION_KEEPALIVE_SECS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(std::time::Duration::from_secs(secs)),
            Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
        },
        Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
    }
}

struct ReconcilerTaskHandle {
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: AbortOnDropHandle<()>,
}

impl ReconcilerTaskHandle {
    fn abort(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }
}

struct StartupRecoveryTaskHandle {
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: AbortOnDropHandle<()>,
}

impl StartupRecoveryTaskHandle {
    fn abort(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }

    async fn wait(self) {
        let _ = self.handle.await;
    }
}

#[derive(Default)]
struct StartupRecoveryState {
    pending: bool,
    handle: Option<StartupRecoveryTaskHandle>,
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct StartupRecoveryProbe {
    entered: AtomicBool,
    dropped: AtomicBool,
    released: AtomicBool,
    entered_notify: tokio::sync::Notify,
    dropped_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-support"))]
impl StartupRecoveryProbe {
    pub fn new() -> Self {
        Self {
            entered: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            released: AtomicBool::new(false),
            entered_notify: tokio::sync::Notify::new(),
            dropped_notify: tokio::sync::Notify::new(),
            release_notify: tokio::sync::Notify::new(),
        }
    }

    pub async fn wait_until_entered(&self) {
        while !self.entered.load(Ordering::SeqCst) {
            self.entered_notify.notified().await;
        }
    }

    pub async fn wait_until_dropped(&self) {
        while !self.dropped.load(Ordering::SeqCst) {
            self.dropped_notify.notified().await;
        }
    }

    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release_notify.notify_waiters();
    }

    async fn pause(self: Arc<Self>) {
        struct DropGuard {
            probe: Arc<StartupRecoveryProbe>,
        }

        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.probe.dropped.store(true, Ordering::SeqCst);
                self.probe.dropped_notify.notify_waiters();
            }
        }

        let _guard = DropGuard {
            probe: Arc::clone(&self),
        };
        self.entered.store(true, Ordering::SeqCst);
        self.entered_notify.notify_waiters();
        while !self.released.load(Ordering::SeqCst) {
            self.release_notify.notified().await;
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for StartupRecoveryProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct StartupRecoveryProbeGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for StartupRecoveryProbeGuard {
    fn drop(&mut self) {
        *STARTUP_RECOVERY_PROBE.lock().unwrap() = None;
    }
}

#[cfg(any(test, feature = "test-support"))]
static STARTUP_RECOVERY_PROBE: Mutex<Option<Arc<StartupRecoveryProbe>>> = Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
async fn pause_startup_recovery_if_probed() {
    let probe = STARTUP_RECOVERY_PROBE.lock().unwrap().clone();
    if let Some(probe) = probe {
        probe.pause().await;
    }
}

#[cfg(test)]
const PRODUCER_MAX_FIELD_BYTES: usize = 8192;
const MCP_NOT_LICENSED_ERROR_CODE: i32 = -32041;

// ─── JSON-RPC types ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcError {
    fn into_mcp_error(self) -> McpError {
        McpError::new(
            rmcp::model::ErrorCode(self.code as i32),
            self.message,
            self.data,
        )
    }
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
    fn invalid_params(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32602, msg)
    }

    fn internal_error(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32603, msg)
    }

    fn mcp_error(id: Value, error: McpError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: i64::from(error.code.0),
                message: error.message.into_owned(),
                data: error.data,
            }),
        }
    }
}

pub(crate) fn require_feature(
    key: FeatureKey,
    feature_gate: &spur_license::FeatureGate,
) -> Result<(), McpError> {
    if feature_gate.has(key) {
        return Ok(());
    }

    Err(McpError::new(
        rmcp::model::ErrorCode(MCP_NOT_LICENSED_ERROR_CODE),
        format!("not licensed for feature {}", key.as_str()),
        Some(json!({
            "reason": "not_licensed",
            "feature": key.as_str(),
            "required_tier": "pro"
        })),
    ))
}

pub(crate) fn feature_error_message(error: McpError) -> String {
    error.message.into_owned()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectionError {
    #[error("stored blob is not valid UTF-8: {0}")]
    InvalidUtf8(#[source] std::string::FromUtf8Error),
    #[error("stored blob is not a valid DelegationResult: {0}")]
    InvalidResult(#[source] serde_json::Error),
    #[error("projection serialization failed: {0}")]
    SerializeFailed(#[source] serde_json::Error),
}

pub(crate) fn project_section(
    full_bytes: &[u8],
    section: spur_blob_store::Section,
    key: &spur_acp::domain::outcome::OutcomeKey,
) -> std::result::Result<String, ProjectionError> {
    use spur_acp::domain::DelegationResult;
    use spur_blob_store::Section;

    if matches!(section, Section::Full) {
        return String::from_utf8(full_bytes.to_vec()).map_err(ProjectionError::InvalidUtf8);
    }

    let result: DelegationResult =
        serde_json::from_slice(full_bytes).map_err(ProjectionError::InvalidResult)?;
    let estimated_cost_micros =
        crate::outcome_materializer::usd_to_micros_saturating(result.estimated_cost_usd);

    let projected = match section {
        Section::StatusOnly => json!({
            "status": result.status,
            "attempt": key.attempt,
            "brain_session": &key.brain_session_id,
            "estimated_cost_micros": estimated_cost_micros,
        }),
        Section::Summary => json!({
            "status": result.status,
            "attempt": key.attempt,
            "brain_session": &key.brain_session_id,
            "summary": result.summary,
            "estimated_cost_micros": estimated_cost_micros,
        }),
        Section::DiffOnly => json!({
            "status": result.status,
            "diff": result.diff,
            "diff_summary": result.diff_summary,
        }),
        Section::Full => unreachable!("handled above"),
    };

    serde_json::to_string(&projected).map_err(ProjectionError::SerializeFailed)
}

/// Convert a `DelegationDispatchError` (defined in `spur-acp`) into the
/// crate-local `JsonRpcResponse`. Lives here because `JsonRpcResponse`
/// is private to this crate and the orphan rules forbid an inherent
/// `impl` on the foreign enum.
fn dispatch_error_response(err: DelegationDispatchError, id: Value) -> JsonRpcResponse {
    JsonRpcResponse::error(id, err.json_rpc_code(), err.to_string())
}

// ─── Worker info (static data set at startup) ─────────────────────────

/// Descriptor for a worker-capable agent, returned by the
/// `list_available_workers` MCP tool.
///
/// Populated by `build_worker_info` from a merged `AgentConfig`.
/// See design spec section C.1.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerInfo {
    pub name: String,
    pub tier: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub good_for: Vec<String>,
    #[serde(default)]
    pub avoid_for: Vec<String>,
    pub output_shape: Option<String>,
    pub cost_tier: Option<String>,
}

/// Build the public `WorkerInfo` from a merged `AgentConfig`.
/// Call AFTER `apply_builtin_defaults` to see inherited values.
pub fn build_worker_info(cfg: &spur_acp::config::AgentConfig) -> WorkerInfo {
    use spur_acp::config::Tier;
    WorkerInfo {
        name: cfg.name.clone(),
        tier: cfg.delegation.tier.map(|t| match t {
            Tier::Specialist => "specialist".into(),
            Tier::Generalist => "generalist".into(),
        }),
        description: cfg.delegation.description.clone(),
        good_for: cfg.delegation.good_for.clone(),
        avoid_for: cfg.delegation.avoid_for.clone(),
        output_shape: cfg.delegation.output_shape.clone(),
        cost_tier: Some(format!("{:?}", cfg.cost_tier).to_lowercase()),
    }
}

// ─── Detached continuation types ─────────────────────────────────────

/// Boxed async callback invoked by `spawn_result_collector` when a detached
/// delegation finishes.
///
/// Arguments:
/// - `BrainContinuation` — the completed delegation result.
/// - `String` — worker-session identifier (delegation UUID used as proxy for
///   the `DelegationCompleted` UI event; unique per delegation).
///
/// Implementer routes the continuation back to the orchestrator ingress
/// (emit UI event first, then try_send / overflow — INV-C3).
pub type DetachedCompletionCallback = Arc<
    dyn Fn(
            spur_acp::domain::BrainContinuation,
            String, // worker_session proxy
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Bundle of handles required to funnel detached delegation completions back
/// into the orchestrator's ingress channel.
///
/// Uses a boxed async callback so that `spur-mcp` does not need to depend on
/// `spur-core` (which would create a circular dependency).  `spur-core` wires
/// the real `report_detached_completion` implementation in
/// `Orchestrator::build_continuation_ctx`.
pub struct DetachedContinuationCtx {
    /// See [`DetachedCompletionCallback`] for the callback contract.
    pub on_complete: DetachedCompletionCallback,
}

/// Why a delegation went detached (used to set `ContinuationSource`).
pub enum DetachedSourceKind {
    AsyncRequested,
    BlockTimeout,
}

/// All the handles `spawn_result_collector` needs to call
/// `report_detached_completion` when a detached delegation finishes.
pub struct DetachedCompletionHandle {
    pub ctx: Arc<DetachedContinuationCtx>,
    pub source_kind: DetachedSourceKind,
    pub attempt_tracker: Arc<AtomicU32>,
    pub brain_session: SessionId,
    pub event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pub materializer: OutcomeMaterializer,
}

fn new_attempt_tracker() -> Arc<AtomicU32> {
    Arc::new(AtomicU32::new(1))
}

/// Phase 3 (plan-5 §7.3): the materializer is the single producer of
/// `BrainContinuation` for completed delegations. This function is now a
/// thin wrapper that forwards to `OutcomeMaterializer::materialize`.
pub(crate) async fn build_detached_continuation(
    delegation_id: &DelegationId,
    result: &DelegationResult,
    source: spur_acp::domain::ContinuationSource,
    attempt: u32,
    brain_session: SessionId,
    event_sink: Option<&Arc<dyn crate::events::McpEventSink>>,
    materializer: &OutcomeMaterializer,
) -> spur_acp::domain::BrainContinuation {
    let brain_session_id = spur_acp::BrainSessionId::new(brain_session);
    materializer
        .materialize(
            result.clone(),
            delegation_id.clone(),
            attempt,
            brain_session_id,
            source,
            event_sink,
        )
        .await
}

// ─── McpCallbackServer ───────────────────────────────────────────────

/// MCP callback server that brain agents connect to via HTTP.
///
/// Exposes delegation and PM tools via JSON-RPC over HTTP POST,
/// compatible with the MCP Streamable HTTP transport.
pub struct McpCallbackServer {
    /// Channel to send delegation requests to the orchestrator.
    delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Available worker agents (set once at creation).
    workers: Vec<WorkerInfo>,
    /// Brain session this server belongs to. INV-2: typed as BrainSessionId.
    brain_session_id: Arc<OnceCell<spur_acp::BrainSessionId>>,
    brain_session_id_notify: Arc<tokio::sync::Notify>,
    /// Delegation IDs whose background collector is still awaiting a result.
    active_delegations: Arc<tokio::sync::Mutex<HashSet<DelegationId>>>,
    /// Results that a background collector has received but the brain has
    /// not yet polled via `check_delegation_status`. Stored with insertion
    /// timestamp for TTL-based lazy eviction. Phase 4: normally empty —
    /// `BlockTimeout` collectors skip the write (INV-ASYNC-2) and the
    /// `AsyncRequested` path retired with `delegate_async` / `wait_delegation`.
    /// Retained as a TTL-bounded debug-injection buffer.
    completed_delegations:
        Arc<tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>>,
    /// Tracks spawned result-collector tasks for graceful shutdown.
    task_tracker: TaskTracker,
    /// Optional PM service for direct issue/PR operations.
    pm_service: Option<Arc<PmService>>,
    /// Optional event sink for emitting MCP lifecycle events.
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    /// Feature gate snapshot shared with the orchestrator/license runtime.
    feature_gate: Arc<spur_license::FeatureGate>,
    /// Active execution plans submitted via `submit_plan`.
    active_plans:
        Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::plan::PlanState>>>>>,
    /// Ephemeral reconciler outcome buffers. MUST NOT be persisted to beads;
    /// durable plan state is reconstructed from beads on restart.
    reconciler_outcomes: Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>>,
    /// Phase 2.5 idempotency guard: maps `epic_id → plan_id` for the
    /// currently-active plan (if any). A sentinel `"__pending__"` value is
    /// used briefly during the PmService fetch to prevent concurrent
    /// `execute_epic` calls from racing into double-dispatch. Terminal plans
    /// are cleared lazily on the next `execute_epic` call for the same epic.
    plan_registry: Arc<tokio::sync::Mutex<crate::plan::PlanRegistry>>,
    /// Serializes current-brain ownership claims across `execute_epic` and
    /// `resume_plan`. The durable invariant lives in beads owner labels; this
    /// local lock closes scan-before-write races within one brain server.
    active_plan_claim_lock: Arc<tokio::sync::Mutex<()>>,
    /// INV-6: handle to the orchestrator's per-delegation cancellation token
    /// registry. `None` in test harnesses that don't wire a real orchestrator.
    cancellation_control: Option<CancellationControl>,
    /// Bundle of handles for routing detached delegation completions back
    /// into the orchestrator ingress via `report_detached_completion`.
    continuation_ctx: Arc<DetachedContinuationCtx>,
    pub(crate) materializer: OutcomeMaterializer,
    pub(crate) outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    /// Phase 1c: how long `handle_delegate_to_worker` / `handle_delegate_parallel`
    /// wait inline for a worker's oneshot to fire before handing the receiver
    /// to the detached collector. Default `0` — pure async-first.
    /// Wired from `SpurConfig.delegation.inline_wait_ms`.
    inline_wait: std::time::Duration,
    /// v3-c: set by `mark_retiring`; delegation entry points reject new
    /// requests once retirement begins.
    retiring: AtomicBool,
    /// v3-c: parent cancellation token for in-flight collector tasks.
    cancel_token: CancellationToken,
    /// v3-c: handle to the root listener task so `force_abort` can stop it.
    root_handle: Mutex<Option<JoinHandle<()>>>,
    /// Handle to the optional beads reconciler task. It is enabled only after
    /// the orchestrator binds this server to a derived brain_session_id.
    reconciler_handle: Mutex<Option<ReconcilerTaskHandle>>,
    /// Optional startup recovery for legacy persisted plans. `start()` only
    /// decides whether it is needed; the task is spawned after the server has
    /// a brain_session_id so recovery can be owner-aware.
    startup_recovery: Mutex<StartupRecoveryState>,
    /// v0a.3: if true, `enable_reconciler` may spawn the reconciler after
    /// the brain_session_id is bound. Wired via `set_reconciler_enabled`.
    reconciler_enabled: bool,
    /// Fast-forward trigger for the reconciler. When the plan executor completes
    /// a task (transitions to AwaitingReview), it notifies the reconciler so it can
    /// immediately tick instead of waiting for the next interval. Only meaningful
    /// when `reconciler_enabled` is true.
    reconciler_fast_forward: Option<Arc<tokio::sync::Notify>>,
    /// Repository root for constructing paths used by beads-backed startup and
    /// plan automation. Set by `set_repo_root` before `start()`.
    repo_root: Option<std::path::PathBuf>,
    /// v0e: opt-in auto-merge/PR on durable epic completion.
    auto_merge_approved_plans: bool,
    /// Grace period before startup quarantines stale `spur:plan-pending`
    /// persisted-plan epics.
    plan_pending_grace: std::time::Duration,
    /// Duration written into `spur:lease-expires-at:<ts>` labels for
    /// reconciler-owned persisted-plan dispatches.
    dispatch_lease_duration: std::time::Duration,
}

/// Validate args for `delegate_parallel` beyond what the schema shape
/// enforces. Currently: per-task `issue_id` values must be pairwise
/// unique across the batch when non-null. Public (crate-level) for
/// integration test access.
pub fn validate_parallel_args(args: &Value) -> Result<(), String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (idx, task) in tasks.iter().enumerate() {
        if let Some(id) = task.get("issue_id").and_then(|v| v.as_str()) {
            if !seen.insert(id) {
                return Err(format!(
                    "delegate_parallel: issue_id values must be unique across tasks (duplicate '{id}' at index {idx})",
                ));
            }
        }
    }
    Ok(())
}

/// Build the embedded Community-tier feature gate used by fallback and tests.
pub fn community_feature_gate() -> Arc<spur_license::FeatureGate> {
    Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ))
}

#[cfg(test)]
fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ));
    let features =
        std::collections::BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        features,
    ));
    gate
}

/// Parse the `tasks` array from a `delegate_parallel` args payload into
/// a list of partially-populated `DelegationRequest` skeletons. Public
/// (crate-level) so integration tests can exercise the parse logic
/// without a live MCP session.
///
/// The returned requests have dummy oneshot senders — do not dispatch
/// them; they are for field-value assertions only.
pub fn parse_parallel_tasks(
    args: &Value,
    brain_session_id: &spur_acp::BrainSessionId,
) -> Result<Vec<DelegationRequest>, String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut out = Vec::with_capacity(tasks.len());
    for task_obj in tasks {
        let task: crate::tool_schemas::DelegateParallelTaskInput =
            serde_json::from_value(task_obj.clone())
                .map_err(|e| format!("Invalid task arguments: {e}"))?;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        out.push(DelegationRequest {
            id: DelegationId::new(),
            agent: task.agent,
            task: task.task,
            context_files: task.context_files.unwrap_or_default(),
            respond_to: tx,
            brain_session_id: brain_session_id.clone(),
            delegation_plan: task.delegation_plan,
            issue_id: task.issue_id,
            base: task.base,
            dispatched_base_oid_tx: None,
            attempt_tracker: new_attempt_tracker(),
            enable_worker_mcp: task.enable_worker_mcp,
        });
    }
    Ok(out)
}

/// Result of building a beads epic subgraph for a persisted plan.
#[derive(Debug, Clone)]
pub struct EpicSubgraph {
    pub epic_id: String,
    /// Maps each `PlanTask.task_id` → beads child issue ID.
    pub task_map: std::collections::HashMap<String, String>,
}

/// Compose a beads epic + child issues + dependency edges from a
/// validated plan. Labels each child with `spur:plan-id:<plan_id>` so
/// review_task can correlate approvals back to beads.
///
/// Creates issues in topological order (deps-first) so each child's
/// `depends_on` references beads IDs that already exist. Callers must
/// ensure the plan is validated (no cycles) before invoking.
///
/// On failure mid-creation: partial state lands in beads (epic +
/// whatever children succeeded), but the epic keeps `spur:plan-pending`
/// and never gains `spur:plan-complete`, so the reconciler will not
/// dispatch the partial graph. Startup sweep quarantines stale pending
/// graphs after the configured grace period.
pub async fn build_epic_subgraph(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<EpicSubgraph, String> {
    build_epic_subgraph_with_activation_labels(
        pm,
        feature_gate,
        plan_id,
        epic_title,
        epic_body,
        tasks,
        Vec::new(),
    )
    .await
}

async fn build_epic_subgraph_with_activation_labels(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
    activation_add_labels: Vec<String>,
) -> Result<EpicSubgraph, String> {
    let (epic_create, child_specs) =
        plan_epic_issue_creates(plan_id, epic_title, epic_body, tasks)?;
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let advanced = pm.advanced();

    let epic_id = pm
        .create_issue(epic_create)
        .await
        .map_err(|e| format!("failed to create beads epic: {e}"))?;

    let mut task_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (task_id, mut child_create) in child_specs {
        // Rewrite `depends_on` from task_id keys → created beads IDs.
        child_create.depends_on = child_create
            .depends_on
            .iter()
            .map(|dep_key| {
                task_map.get(dep_key).cloned().ok_or_else(|| {
                    format!("task '{task_id}' depends on '{dep_key}' which was not yet created",)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        child_create.parent = Some(epic_id.clone());

        let child_id = pm
            .create_issue(child_create)
            .await
            .map_err(|e| format!("failed to create child for task '{task_id}': {e}"))?;
        let task = tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .ok_or_else(|| format!("task spec for '{task_id}' disappeared during persistence"))?;
        if !task.context_files.is_empty() {
            let adv = advanced.ok_or_else(|| {
                format!(
                    "failed to persist child task spec for task '{task_id}': beads backend missing"
                )
            })?;
            crate::plan::emit_task_spec_audit(adv, &child_id, &task.task_id, &task.context_files)
                .await
                .map_err(|e| {
                    format!("failed to persist child task spec for task '{task_id}': {e}")
                })?;
        }
        task_map.insert(task_id, child_id);
    }

    let mut add_labels = activation_add_labels;
    add_labels.push(crate::plan::labels::PLAN_COMPLETE.to_string());
    pm.update_issue(
        &epic_id,
        spur_pm::types::IssueUpdate {
            add_labels,
            remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
            ..Default::default()
        },
    )
    .await
    .map_err(|e| {
        format!(
            "failed to activate beads epic '{epic_id}' (add {} / remove {}): {e}",
            crate::plan::labels::PLAN_COMPLETE,
            crate::plan::labels::PLAN_PENDING
        )
    })?;

    Ok(EpicSubgraph { epic_id, task_map })
}

/// Emit a `[[spur-audit v1]]` `PlanSubmit` sentinel comment on the epic issue.
///
/// Advisory: failure is logged via `tracing::warn!` and swallowed. Does not
/// abort the caller. See docs/superpowers/plans/2026-04-20-adaptive-plan-
/// repair-v0a.md "Review addendum II" for why comments are the audit
/// transport.
pub async fn emit_plan_submit_audit(
    advanced: &dyn spur_pm::BeadsAdvanced,
    plan_id: &str,
    sg: &EpicSubgraph,
    base_snapshot_branch: Option<&str>,
    base_snapshot_oid: Option<&str>,
    execution_mode: Option<&str>,
    brain_session_id: Option<&SessionId>,
    explicit_base: Option<&crate::tools::BaseTarget>,
) {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
        plan_id: plan_id.to_string(),
        epic_issue_id: sg.epic_id.clone(),
        task_ids: sg.task_map.values().cloned().collect(),
        base_snapshot_branch: base_snapshot_branch.map(str::to_string),
        base_snapshot_oid: base_snapshot_oid.map(str::to_string),
        execution_mode: execution_mode.map(str::to_string),
        brain_session_id: brain_session_id.map(ToString::to_string),
        explicit_base: explicit_base.cloned(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = advanced.add_comment(&sg.epic_id, &body).await {
        tracing::warn!(
            target: "spur.audit.emit_failure",
            kind = "plan_submit",
            epic_id = %sg.epic_id,
            plan_id = %plan_id,
            "PlanSubmit audit comment emission failed (graph is persisted; audit missing): {e}"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedPlanBootstrap {
    #[allow(dead_code)]
    pub(crate) epic_id: String,
    pub(crate) base_snapshot_branch: Option<String>,
    pub(crate) base_snapshot_oid: Option<String>,
}

impl PersistedPlanBootstrap {
    pub(crate) fn preferred_base_ref(&self) -> Option<&str> {
        self.base_snapshot_oid
            .as_deref()
            .or(self.base_snapshot_branch.as_deref())
    }
}

pub(crate) async fn read_persisted_plan_bootstrap(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_id: &str,
) -> Result<PersistedPlanBootstrap, String> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted bootstrap recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(epic_id)
        .await
        .map_err(|e| format!("failed to load comments for epic '{epic_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(epic_id, comments);

    audits
        .into_iter()
        .rev()
        .find_map(|audit| match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id: audit_plan_id,
                base_snapshot_branch,
                base_snapshot_oid,
                ..
            } if audit_plan_id == plan_id => Some(PersistedPlanBootstrap {
                epic_id: epic_id.to_string(),
                base_snapshot_branch,
                base_snapshot_oid,
            }),
            _ => None,
        })
        .ok_or_else(|| format!("plan '{plan_id}' has no PlanSubmit audit on epic '{epic_id}'"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveOwnedPlan {
    plan_id: String,
    epic_id: String,
}

async fn find_plan_epic(
    pm: &PmService,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    operation: &str,
) -> Result<IssueSummary, String> {
    let epics = pm
        .list_issues(IssueFilter {
            labels: vec![crate::plan::labels::plan_id(plan_id)],
            issue_type: Some("epic".to_string()),
            include_closed: true,
            limit: Some(10),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("{operation}: failed to find plan: {error}"))?;

    if epics.is_empty() {
        return Err(format!("{operation}: plan not found: {plan_id}"));
    }

    if epics.len() == 1 {
        return Ok(epics.into_iter().next().expect("non-empty epics"));
    }

    if require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate).is_ok() {
        let Some(advanced) = pm.advanced() else {
            return Err(format!(
                "{operation}: ambiguous plan lookup for {plan_id}; beads advanced backend is unavailable"
            ));
        };
        let candidate_ids = epics
            .iter()
            .map(|epic| epic.id.clone())
            .collect::<HashSet<_>>();
        let mut canonical_ids = HashSet::new();

        for epic in &epics {
            match advanced.list_comments(&epic.id).await {
                Ok(comments) => {
                    let audits =
                        crate::plan::projector::collect_sorted_audits_for_issue(&epic.id, comments);
                    for audit in audits {
                        if let crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                            plan_id: audit_plan_id,
                            epic_issue_id,
                            ..
                        } = audit
                        {
                            if audit_plan_id == plan_id && candidate_ids.contains(&epic_issue_id) {
                                canonical_ids.insert(epic_issue_id);
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        epic_id = %epic.id,
                        plan_id = %plan_id,
                        operation = %operation,
                        error = %error,
                        "failed to inspect plan-submit audits while resolving duplicate plan epics"
                    );
                }
            }
        }

        if canonical_ids.len() == 1 {
            let canonical_id = canonical_ids
                .into_iter()
                .next()
                .expect("canonical_ids has one entry");
            if let Some(epic) = epics.iter().find(|epic| epic.id == canonical_id).cloned() {
                tracing::warn!(
                    plan_id = %plan_id,
                    operation = %operation,
                    canonical_epic = %epic.id,
                    "resolved duplicate plan epics via PlanSubmit audit canonical epic"
                );
                return Ok(epic);
            }
        } else if !canonical_ids.is_empty() {
            let mut ids = canonical_ids.into_iter().collect::<Vec<_>>();
            ids.sort();
            return Err(format!(
                "{operation}: ambiguous plan lookup for {plan_id}; PlanSubmit audits disagree on canonical epics: {}",
                ids.join(", ")
            ));
        }
    }

    let epic_ids = epics
        .iter()
        .map(|epic| epic.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "{operation}: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedTaskCompletion {
    pub(crate) worker_branch: Option<String>,
    pub(crate) summary: Option<String>,
}

pub(crate) async fn read_latest_task_completion(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
) -> Result<Option<PersistedTaskCompletion>, String> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted task completion recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(issue_id)
        .await
        .map_err(|e| format!("failed to load comments for task '{issue_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(issue_id, comments);

    Ok(audits.into_iter().rev().find_map(|audit| match audit {
        crate::plan::audit_sentinel::AuditSentinelKind::Completion {
            worker_branch,
            result_summary,
            ..
        } => Some(PersistedTaskCompletion {
            worker_branch,
            summary: result_summary,
        }),
        _ => None,
    }))
}

pub(crate) async fn reconstruct_historical_attempts(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
    current_attempt: u32,
) -> Result<Vec<crate::plan::AttemptRecord>, String> {
    #[derive(Debug, Default)]
    struct AttemptAccumulator {
        attempt: u32,
        worker_branch: Option<String>,
        summary: Option<String>,
        feedback: String,
    }

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted attempt recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(issue_id)
        .await
        .map_err(|e| format!("failed to load comments for task '{issue_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(issue_id, comments);

    let mut attempts_by_delegation: std::collections::HashMap<String, AttemptAccumulator> =
        std::collections::HashMap::new();
    for audit in audits {
        match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                delegation_id,
                attempt,
                ..
            } if attempt < current_attempt => {
                attempts_by_delegation
                    .entry(delegation_id)
                    .or_insert_with(|| AttemptAccumulator {
                        attempt,
                        ..Default::default()
                    });
            }
            crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                delegation_id,
                worker_branch,
                result_summary,
                ..
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.worker_branch = worker_branch;
                    record.summary = result_summary;
                }
            }
            crate::plan::audit_sentinel::AuditSentinelKind::Rejection {
                delegation_id,
                feedback,
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.feedback = feedback;
                }
            }
            // bd-33it: request_changes feedback also populates the historical
            // attempt record. Joined by delegation_id so the get_task_diff
            // operator-visible historical view sees the same feedback the
            // reconciler used to enrich the worker prompt on retry.
            crate::plan::audit_sentinel::AuditSentinelKind::ReviewFeedback {
                delegation_id,
                feedback,
                worker_branch,
                summary,
                ..
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.feedback = feedback;
                    if record.worker_branch.is_none() {
                        record.worker_branch = worker_branch;
                    }
                    if record.summary.is_none() {
                        record.summary = summary;
                    }
                }
            }
            _ => {}
        }
    }

    let mut history: Vec<crate::plan::AttemptRecord> = attempts_by_delegation
        .into_values()
        .map(|record| crate::plan::AttemptRecord {
            attempt: record.attempt,
            worker_branch: record.worker_branch,
            diff_summary: None,
            summary: record.summary,
            feedback: record.feedback,
            dispatched_base_oid: None,
        })
        .collect();
    history.sort_by_key(|record| record.attempt);
    Ok(history)
}

async fn apply_issue_update(
    pm: &spur_pm::PmService,
    issue_id: &str,
    mut update: spur_pm::IssueUpdate,
) -> anyhow::Result<()> {
    let core_update = spur_pm::IssueUpdate {
        status: update.status.take(),
        comment: update.comment.take(),
        priority: update.priority.take(),
        assignee: update.assignee.take(),
        ..Default::default()
    };
    if core_update.status.is_some()
        || core_update.comment.is_some()
        || core_update.priority.is_some()
        || core_update.assignee.is_some()
    {
        pm.update_issue(issue_id, core_update).await?;
    }

    if !update.add_labels.is_empty() || !update.remove_labels.is_empty() {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: update.add_labels,
                remove_labels: update.remove_labels,
                ..Default::default()
            },
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
fn discover_plan_ids(issues: &[spur_pm::IssueSummary]) -> Vec<String> {
    let mut plan_ids = std::collections::BTreeSet::new();
    for issue in issues {
        if issue.status != "open" || issue.issue_type.as_deref() != Some("epic") {
            continue;
        }
        if issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
            || !issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        {
            continue;
        }
        for label in &issue.labels {
            if let Some(plan_id) = crate::plan::labels::parse_plan_id(label) {
                plan_ids.insert(plan_id.to_string());
            }
        }
    }
    plan_ids.into_iter().collect()
}

fn discover_plan_ids_owned_by(
    issues: &[spur_pm::IssueSummary],
    current_brain_session: &spur_acp::SessionId,
) -> Vec<String> {
    let mut plan_ids = std::collections::BTreeSet::new();
    for issue in issues {
        if issue.status != "open" || issue.issue_type.as_deref() != Some("epic") {
            continue;
        }
        if issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
            || !issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        {
            continue;
        }
        match crate::plan::ownership::classify_owner(&issue.labels, current_brain_session) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                tracing::debug!(
                    epic_id = %issue.id,
                    %owner,
                    "startup recovery skipped plan owned by another brain"
                );
                continue;
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                tracing::debug!(
                    epic_id = %issue.id,
                    owner = %owners.join(","),
                    "startup recovery skipped plan with ambiguous owner labels"
                );
                continue;
            }
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                tracing::debug!(
                    epic_id = %issue.id,
                    "startup recovery skipped unowned plan"
                );
                continue;
            }
        }
        for label in &issue.labels {
            if let Some(plan_id) = crate::plan::labels::parse_plan_id(label) {
                plan_ids.insert(plan_id.to_string());
            }
        }
    }
    plan_ids.into_iter().collect()
}

fn mutation_orphan_ids(audits: &[crate::plan::audit_sentinel::AuditSentinelKind]) -> Vec<String> {
    let planned: std::collections::BTreeSet<String> = audits
        .iter()
        .filter_map(|audit| {
            if let crate::plan::audit_sentinel::AuditSentinelKind::MutationPlan {
                mutation_id,
                ..
            } = audit
            {
                Some(mutation_id.clone())
            } else {
                None
            }
        })
        .collect();
    let terminal: std::collections::BTreeSet<String> = audits
        .iter()
        .filter_map(|audit| match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::MutationCommit {
                mutation_id, ..
            } => Some(mutation_id.clone()),
            crate::plan::audit_sentinel::AuditSentinelKind::MutationInvariantViolation {
                mutation_id,
                ..
            } => Some(mutation_id.clone()),
            _ => None,
        })
        .collect();

    planned.difference(&terminal).cloned().collect()
}

fn replace_execution_labels(
    issue: &spur_pm::Issue,
    plan_id: &str,
    agent_name: &str,
) -> spur_pm::IssueUpdate {
    let add_labels = vec![
        crate::plan::labels::plan_id(plan_id),
        crate::plan::labels::agent(agent_name),
    ];
    let mut remove_labels = Vec::new();
    for label in &issue.labels {
        if crate::plan::labels::parse_plan_id(label).is_some()
            || crate::plan::labels::parse_agent(label).is_some()
        {
            remove_labels.push(label.clone());
        }
    }
    filter_remove_labels(&mut remove_labels, &add_labels);

    spur_pm::IssueUpdate {
        add_labels,
        remove_labels,
        ..Default::default()
    }
}

fn replace_task_execution_labels(
    issue: &spur_pm::Issue,
    plan_id: &str,
    task_id: &str,
    agent_name: &str,
) -> spur_pm::IssueUpdate {
    let mut update = replace_execution_labels(issue, plan_id, agent_name);
    update
        .add_labels
        .push(crate::plan::labels::plan_task_id(task_id));
    for label in &issue.labels {
        if crate::plan::labels::parse_plan_task_id(label).is_some() {
            update.remove_labels.push(label.clone());
        }
    }
    filter_remove_labels(&mut update.remove_labels, &update.add_labels);
    update
}

/// Drop any label from `remove_labels` that also appears in `add_labels`.
///
/// The beads CLI processes adds before removes, so an "add X then remove X"
/// pair on the same issue would strip a label we just (idempotently) added.
/// Filter the no-op pair out before issuing the update.
fn filter_remove_labels(remove_labels: &mut Vec<String>, add_labels: &[String]) {
    let add_set: std::collections::HashSet<&str> = add_labels.iter().map(String::as_str).collect();
    remove_labels.retain(|label| !add_set.contains(label.as_str()));
}

fn persisted_plan_epic_plan_id(issue: &spur_pm::Issue) -> Option<&str> {
    if issue.issue_type.as_deref() != Some("epic") {
        return None;
    }

    let is_persisted_plan_scope = issue
        .labels
        .iter()
        .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        || issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING);
    if !is_persisted_plan_scope {
        return None;
    }

    issue
        .labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_id(label))
}

fn invert_label_update(update: &spur_pm::IssueUpdate) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        add_labels: update.remove_labels.clone(),
        remove_labels: update.add_labels.clone(),
        ..Default::default()
    }
}

fn legacy_reclaim_needed(has_rev1_merge_base_metadata: bool) -> bool {
    !has_rev1_merge_base_metadata
}

async fn any_open_epic_lacks_rev1_metadata(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
) -> anyhow::Result<bool> {
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

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let Some(adv) = pm.advanced() else {
        return Ok(false);
    };

    for epic in &epics {
        if epic
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
        {
            continue;
        }
        if let Some(plan_id) = epic
            .labels
            .iter()
            .find_map(|l| crate::plan::labels::parse_plan_id(l))
        {
            let comments = adv.list_comments(&epic.id).await?;
            let audits =
                crate::plan::projector::collect_sorted_audits_for_issue(&epic.id, comments);
            let has_rev1_metadata = audits.iter().any(|audit| {
                matches!(
                    audit,
                    crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                        plan_id: pid,
                        base_snapshot_branch: Some(_),
                        base_snapshot_oid: Some(_),
                        ..
                    } if pid == plan_id
                )
            });
            if !has_rev1_metadata {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[doc(hidden)]
pub async fn compensate_mutation_orphans(
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    task_id: &str,
) -> anyhow::Result<()> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate.as_ref())
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("mutation recovery requires beads backend"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        task_id,
        adv.list_comments(task_id).await?,
    );

    for mutation_id in mutation_orphan_ids(&audits) {
        if let Ok(uuid) = uuid::Uuid::parse_str(&mutation_id) {
            let mutation_label = crate::plan::labels::mutation_id_label(&uuid);
            let summaries = pm
                .list_issues(spur_pm::IssueFilter {
                    labels: vec![mutation_label],
                    limit: Some(1_000),
                    ..Default::default()
                })
                .await?;
            let child_ids: Vec<String> = summaries.into_iter().map(|summary| summary.id).collect();
            for child_id in &child_ids {
                pm.update_issue(
                    child_id,
                    spur_pm::IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        ..Default::default()
                    },
                )
                .await?;
            }
            apply_issue_update(
                pm.as_ref(),
                task_id,
                spur_pm::IssueUpdate {
                    status: Some("open".to_string()),
                    remove_labels: crate::plan::labels::superseded_by_labels(&child_ids),
                    ..Default::default()
                },
            )
            .await?;
        }

        adv.add_comment(
            task_id,
            &crate::plan::audit_sentinel::encode_comment(
                &crate::plan::audit_sentinel::AuditSentinelKind::MutationInvariantViolation {
                    mutation_id: mutation_id.clone(),
                    violation: "restart-orphan".into(),
                    rollback_status: "compensated".into(),
                    rollback_ops_succeeded: Vec::new(),
                    rollback_ops_failed: Vec::new(),
                },
            ),
        )
        .await?;
    }
    Ok(())
}

#[doc(hidden)]
pub async fn resolve_dispatch_orphan(
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    task_id: &str,
) -> anyhow::Result<bool> {
    let issue = pm.get_issue(task_id).await?;
    if issue.status != "open" {
        return Ok(false);
    }
    let Some(delegation_id) = issue.labels.iter().find_map(|label| {
        crate::plan::labels::parse_delegation_id(label)
            .or_else(|| label.strip_prefix("delegation-id:"))
    }) else {
        return Ok(false);
    };
    if crate::plan::projector::has_ready_for_review_label_compat(&issue.labels) {
        return Ok(false);
    }

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate.as_ref())
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("dispatch recovery requires beads backend"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        task_id,
        adv.list_comments(task_id).await?,
    );
    if audits.iter().any(|audit| matches!(
        audit,
        crate::plan::audit_sentinel::AuditSentinelKind::Completion { delegation_id: did, .. } if did == delegation_id
    )) {
        return Ok(false);
    }

    adv.add_comment(
        task_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: delegation_id.to_string(),
                reason: ORPHAN_CLEAR_REASON_RESTART.into(),
            },
        ),
    )
    .await?;
    crate::plan::clear_dispatch_intent(pm.as_ref(), task_id, delegation_id).await?;
    Ok(true)
}

/// Pure helper: compute the IssueCreate values that build_epic_subgraph
/// would dispatch to PmService. Returns the epic's IssueCreate plus a
/// Vec of (task_id, IssueCreate) for each child in topological order.
/// Child IssueCreate.depends_on carries task_id keys, NOT beads IDs —
/// the caller rewrites them as children are created.
pub fn plan_epic_issue_creates(
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<
    (
        spur_pm::types::IssueCreate,
        Vec<(String, spur_pm::types::IssueCreate)>,
    ),
    String,
> {
    let epic_create = spur_pm::types::IssueCreate {
        title: epic_title.to_string(),
        description: epic_body.map(String::from),
        issue_type: Some("epic".to_string()),
        labels: vec![
            crate::plan::labels::plan_id(plan_id),
            crate::plan::labels::PLAN_PENDING.to_string(),
        ],
        ..Default::default()
    };

    let order = topological_order(tasks)?;
    let mut child_specs = Vec::with_capacity(tasks.len());
    for idx in order {
        let task = &tasks[idx];
        let mut labels = vec![
            crate::plan::labels::plan_id(plan_id),
            crate::plan::labels::plan_task_id(&task.task_id),
            crate::plan::labels::agent(&task.agent),
        ];
        if let Some(existing) = &task.issue_id {
            labels.push(crate::plan::labels::source_issue(existing));
        }
        let child_create = spur_pm::types::IssueCreate {
            title: format!("{}: {}", task.task_id, truncate_for_title(&task.task)),
            description: Some(task.task.clone()),
            issue_type: Some("task".to_string()),
            labels,
            // depends_on carries task_id keys; rewritten by build_epic_subgraph.
            depends_on: task.depends_on.clone(),
            // parent set by build_epic_subgraph once epic_id is known.
            parent: None,
            ..Default::default()
        };
        child_specs.push((task.task_id.clone(), child_create));
    }
    Ok((epic_create, child_specs))
}

/// Build `PlanTaskEntry` values from a list of `PlanTask`s, optionally
/// backfilling `spec.issue_id` from a `task_map` produced by
/// `build_epic_subgraph`.
///
/// Backfill rule: a task's `issue_id` is set to the task_map value ONLY when
/// the field is currently `None`. Pre-existing values are NOT overwritten —
/// they represent a `spur:source-issue:` reference pointing to a pre-existing
/// issue and must be preserved so downstream audit logic can distinguish the
/// source issue from the newly-created beads child.
///
/// Ephemeral plans pass `task_map = None`; every entry keeps `issue_id: None`.
pub fn build_entries_with_task_map(
    tasks: Vec<crate::plan::PlanTask>,
    task_map: Option<&std::collections::HashMap<String, String>>,
) -> Vec<crate::plan::PlanTaskEntry> {
    tasks
        .into_iter()
        .map(|mut spec| {
            if spec.issue_id.is_none() {
                if let Some(map) = task_map {
                    if let Some(beads_id) = map.get(&spec.task_id) {
                        spec.issue_id = Some(beads_id.clone());
                    }
                }
            }
            crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }
        })
        .collect()
}

/// Truncate a task description to a reasonable issue-title length.
/// Beads has no hard limit but overly long titles are unwieldy in UIs.
fn truncate_for_title(s: &str) -> String {
    const MAX_TITLE_LEN: usize = 80;
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= MAX_TITLE_LEN {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(MAX_TITLE_LEN - 3).collect();
        format!("{truncated}...")
    }
}

/// Return task indices in a valid topological order. Callers must have
/// already validated that the plan is acyclic via `plan::validate_plan`.
fn topological_order(tasks: &[crate::plan::PlanTask]) -> Result<Vec<usize>, String> {
    use std::collections::HashMap;
    let key_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();

    let mut in_degree: Vec<usize> = tasks.iter().map(|t| t.depends_on.len()).collect();
    let mut ready: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| if d == 0 { Some(i) } else { None })
        .collect();

    let mut out = Vec::with_capacity(tasks.len());
    while let Some(i) = ready.pop_front() {
        out.push(i);
        for (j, t) in tasks.iter().enumerate() {
            if t.depends_on
                .iter()
                .any(|dep| key_to_idx.get(dep.as_str()).copied() == Some(i))
            {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    if out.len() != tasks.len() {
        return Err(format!(
            "topological order incomplete: {} of {} tasks reachable (cycle?)",
            out.len(),
            tasks.len()
        ));
    }
    Ok(out)
}

async fn resolve_plan_base(
    repo_root: Option<&std::path::PathBuf>,
    base_target: Option<&crate::tools::BaseTarget>,
) -> Result<PlanBaseSnapshot, String> {
    let Some(root) = repo_root.cloned() else {
        return Ok(PlanBaseSnapshot::default());
    };
    let manager = WorktreeManager::new(root);

    let branch = match base_target {
        // Legacy / explicit RepoMain: snapshot the brain working tree.
        None | Some(crate::tools::BaseTarget::RepoMain) => manager
            .snapshot_brain_state()
            .await
            .map_err(|e| format!("failed to snapshot plan base: {e}"))?,
        // Explicit branch: resolve the ref and create a snapshot ref pointed
        // at the same OID. Brain working tree is never touched.
        Some(crate::tools::BaseTarget::Branch { name }) => manager
            .snapshot_at_ref(name)
            .await
            .map_err(|e| format!("failed to resolve plan base branch '{name}': {e}"))?,
        Some(crate::tools::BaseTarget::Commit { oid }) => manager
            .snapshot_at_ref(oid)
            .await
            .map_err(|e| format!("failed to resolve plan base commit '{oid}': {e}"))?,
    };

    let oid = Some(
        run_git_capture(
            &manager.repo_root,
            None,
            &["rev-parse", "--verify", branch.as_str()],
        )
        .await?,
    );
    Ok(PlanBaseSnapshot {
        branch: Some(branch),
        oid,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PlanBaseSnapshot {
    branch: Option<String>,
    oid: Option<String>,
}

#[cfg(test)]
mod resolve_plan_base_tests {
    use super::*;
    use crate::tools::BaseTarget;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
    }

    fn capture(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn seed_repo(repo: &std::path::Path) {
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "seed"]);
    }

    #[tokio::test]
    async fn resolve_plan_base_none_falls_back_to_brain_snapshot() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        let root = dir.path().to_path_buf();

        let snap = resolve_plan_base(Some(&root), None).await.unwrap();
        assert!(snap
            .branch
            .as_deref()
            .unwrap()
            .starts_with("spur/brain-snapshot-"));
        assert_eq!(snap.oid.as_deref(), Some(head_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_branch_target_skips_stash_and_uses_named_branch() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        run_git(dir.path(), &["checkout", "-q", "-b", "phase0"]);
        std::fs::write(dir.path().join("b"), "2").unwrap();
        run_git(dir.path(), &["add", "b"]);
        run_git(dir.path(), &["commit", "-q", "-m", "phase0 work"]);
        let phase0_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);

        // Dirty the WT — must not affect snapshot.
        std::fs::write(dir.path().join("a"), "dirty").unwrap();

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch {
            name: "phase0".into(),
        };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();

        assert_eq!(snap.oid.as_deref(), Some(phase0_oid.as_str()));
        let a_contents = std::fs::read_to_string(dir.path().join("a")).unwrap();
        assert_eq!(a_contents, "dirty", "WT must be untouched");
    }

    #[tokio::test]
    async fn resolve_plan_base_commit_target_uses_oid() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let seed_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("a"), "2").unwrap();
        run_git(dir.path(), &["add", "a"]);
        run_git(dir.path(), &["commit", "-q", "-m", "second"]);
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        assert_ne!(seed_oid, head_oid);

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Commit {
            oid: seed_oid.clone(),
        };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();
        assert_eq!(snap.oid.as_deref(), Some(seed_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_unknown_branch_fails_loudly() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch {
            name: "does-not-exist".into(),
        };
        let err = resolve_plan_base(Some(&root), Some(&target))
            .await
            .unwrap_err();
        assert!(
            err.contains("does-not-exist"),
            "error must mention the bad ref; got: {err}"
        );
    }
}

#[derive(Debug, Default)]
struct ClobberReviewReport {
    signals: Vec<crate::plan::signals::WorkerSignal>,
    warnings: Vec<String>,
}

fn append_review_warning(resp: &mut serde_json::Value, warning: String) {
    if let serde_json::Value::Object(map) = resp {
        match map.get_mut("warnings") {
            Some(serde_json::Value::Array(warnings)) => warnings.push(json!(warning)),
            _ => {
                map.insert("warnings".into(), json!([warning]));
            }
        }
    }
}

pub(crate) async fn run_git_capture(
    repo_root: &std::path::Path,
    cwd: Option<&std::path::Path>,
    args: &[&str],
) -> Result<String, String> {
    let work_dir = cwd.unwrap_or(repo_root);
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| format!("failed to execute git {}: {e}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(crate) async fn diff_text_from_branches(
    repo_root: &std::path::Path,
    base_ref: &str,
    worker_branch: &str,
) -> Result<String, String> {
    let range = format!("{base_ref}..{worker_branch}");
    run_git_capture(repo_root, None, &["diff", range.as_str()]).await
}

async fn integrate_plan_branches(
    repo_root: &std::path::Path,
    base_ref: &str,
    merge_branch: &str,
    ordered_branches: &[(String, String)],
) -> Result<crate::plan::PlanMergeState, String> {
    let integration_root = repo_root
        .join(".spur/merge")
        .join(uuid::Uuid::new_v4().to_string());
    if let Some(parent) = integration_root.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create integration worktree parent: {e}"))?;
    }
    let integration_root_str = integration_root
        .to_str()
        .ok_or_else(|| "integration worktree path is not valid UTF-8".to_string())?;

    run_git_capture(
        repo_root,
        None,
        &[
            "worktree",
            "add",
            integration_root_str,
            "-b",
            merge_branch,
            base_ref,
        ],
    )
    .await?;

    let mut merged_task_ids = Vec::with_capacity(ordered_branches.len());
    for (task_id, worker_branch) in ordered_branches {
        if let Err(err) = run_git_capture(
            repo_root,
            Some(&integration_root),
            &["cherry-pick", worker_branch.as_str()],
        )
        .await
        {
            let conflict_output = run_git_capture(
                repo_root,
                Some(&integration_root),
                &["diff", "--name-only", "--diff-filter=U"],
            )
            .await
            .unwrap_or_default();
            let files: Vec<String> = conflict_output
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            let _ = run_git_capture(
                repo_root,
                Some(&integration_root),
                &["cherry-pick", "--abort"],
            )
            .await;
            let _ = run_git_capture(
                repo_root,
                None,
                &["worktree", "remove", integration_root_str, "--force"],
            )
            .await;
            info!(
                merge_branch = %merge_branch,
                conflict_task_id = %task_id,
                conflict_worker_branch = %worker_branch,
                error = %err,
                "merge_plan detected cherry-pick conflict"
            );
            return Ok(crate::plan::PlanMergeState::Conflict {
                merge_branch: merge_branch.to_string(),
                conflict_task_id: task_id.clone(),
                conflict_worker_branch: worker_branch.clone(),
                merged_task_ids,
                files,
            });
        }
        merged_task_ids.push(task_id.clone());
    }

    run_git_capture(
        repo_root,
        None,
        &["worktree", "remove", integration_root_str, "--force"],
    )
    .await?;

    Ok(crate::plan::PlanMergeState::Succeeded {
        merge_branch: merge_branch.to_string(),
        merged_task_ids,
    })
}

#[cfg(test)]
mod topo_tests {
    use super::topological_order;
    use crate::plan::PlanTask;

    fn t(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.to_string(),
            agent: "x".to_string(),
            task: "body".to_string(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            context_files: Vec::new(),
        }
    }

    #[test]
    fn linear_chain_is_ordered() {
        let tasks = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        let order = topological_order(&tasks).unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn diamond_respects_all_parents() {
        // a → b, a → c, b+c → d
        let tasks = vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["a"]),
            t("d", &["b", "c"]),
        ];
        let order = topological_order(&tasks).unwrap();
        let pos_a = order.iter().position(|&i| i == 0).unwrap();
        let pos_b = order.iter().position(|&i| i == 1).unwrap();
        let pos_c = order.iter().position(|&i| i == 2).unwrap();
        let pos_d = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_a < pos_b && pos_a < pos_c);
        assert!(pos_b < pos_d && pos_c < pos_d);
    }

    #[test]
    fn cycle_is_detected() {
        let tasks = vec![t("a", &["b"]), t("b", &["a"])];
        let err = topological_order(&tasks).unwrap_err();
        assert!(err.contains("incomplete") || err.contains("cycle"));
    }
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

    fn ensure_accepting_delegations(&self) -> std::result::Result<(), DelegationDispatchError> {
        if self.retiring.load(Ordering::SeqCst) {
            Err(DelegationDispatchError::SessionRetiring)
        } else {
            Ok(())
        }
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

    /// Configure persisted dispatch lease duration for reconciler dispatches.
    pub fn set_dispatch_lease_duration(&mut self, duration: std::time::Duration) {
        self.dispatch_lease_duration = duration;
    }

    /// Spawn `run_plan` for an ephemeral plan (no epic_id). Persisted plans
    /// must use the reconciler; this helper is ephemeral-only by construction.
    fn spawn_ephemeral_plan_runner(&self, state: Arc<tokio::sync::Mutex<crate::plan::PlanState>>) {
        let delegation_tx = self.delegation_tx.clone();
        let plan_sink = self.event_sink.clone();
        let plan_pm = self
            .pm_service
            .clone()
            .map(|p| p as Arc<dyn crate::plan::PmLike>);
        self.task_tracker.spawn(crate::plan::run_plan(
            state,
            delegation_tx,
            plan_sink,
            plan_pm,
            self.reconciler_fast_forward.as_ref().cloned(),
            Arc::clone(&self.continuation_ctx),
            Arc::new(self.materializer.clone()),
            Arc::clone(&self.feature_gate),
        ));
    }

    /// Spawn a background task that awaits a delegation oneshot and stores
    /// the result in `completed_delegations` for later polling.
    ///
    /// When `detached` is `Some`, the task additionally calls
    /// `report_detached_completion` to route the result back into the
    /// orchestrator ingress (INV-C3 ordering: UI event BEFORE ingress).
    ///
    /// Exposed for integration tests in sibling crates. Not part of the
    /// stable public API — `#[doc(hidden)]` keeps it out of rustdoc.
    #[doc(hidden)]
    pub fn spawn_result_collector(
        tracker: &TaskTracker,
        delegation_id: DelegationId,
        rx: tokio::sync::oneshot::Receiver<DelegationResult>,
        cancel_token: CancellationToken,
        active: Arc<tokio::sync::Mutex<HashSet<DelegationId>>>,
        completed: Arc<
            tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>,
        >,
        detached: Option<DetachedCompletionHandle>,
    ) {
        tracker.spawn(async move {
            let result = tokio::select! {
                res = rx => match res {
                    Ok(r) => r,
                    Err(_) => DelegationResult {
                        status: DelegationStatus::Failed {
                            error: "Orchestrator disconnected".into(),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                },
                _ = cancel_token.cancelled() => DelegationResult {
                    status: DelegationStatus::Cancelled {
                        reason: "Brain session retiring".into(),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                },
            };
            active.lock().await.remove(&delegation_id);

            // INV-ASYNC-2 (source_kind-gated): the continuation bridge is the
            // SOLE delivery channel for `BlockTimeout` (the async-first
            // path — `delegate_to_worker` auto-reprompt). Writing to the
            // map on that path would let `check_delegation_status` redeliver
            // what the brain already received as a continuation turn — the
            // double-delivery failure mode closed by INV-ASYNC-1.
            //
            // Phase 4: the `AsyncRequested` source_kind is retained only as a
            // legacy-debugging affordance — the `delegate_async` /
            // `wait_delegation` RPCs that drove it were removed in this
            // phase. In production nothing constructs `AsyncRequested`
            // handles, so the map write below is effectively dead code
            // unless a test/injection harness wires one explicitly.
            //
            // `detached = None` is retained as a fallback for unit
            // tests exercising the collector directly.
            let keep_map_entry = match &detached {
                None => true,
                Some(h) => matches!(h.source_kind, DetachedSourceKind::AsyncRequested),
            };
            if keep_map_entry {
                completed.lock().await.insert(
                    delegation_id.clone(),
                    (result.clone(), tokio::time::Instant::now()),
                );
            }

            if let Some(h) = detached {
                let source = if matches!(result.status, DelegationStatus::Cancelled { .. }) {
                    spur_acp::domain::ContinuationSource::Cancelled
                } else {
                    match h.source_kind {
                        DetachedSourceKind::AsyncRequested => {
                            spur_acp::domain::ContinuationSource::AsyncRequested
                        }
                        DetachedSourceKind::BlockTimeout => {
                            spur_acp::domain::ContinuationSource::BlockTimeout
                        }
                    }
                };

                let DetachedCompletionHandle {
                    ctx,
                    attempt_tracker,
                    brain_session,
                    event_sink,
                    materializer,
                    ..
                } = h;
                let attempt = attempt_tracker.load(Ordering::SeqCst);
                let cont = build_detached_continuation(
                    &delegation_id,
                    &result,
                    source,
                    attempt,
                    brain_session,
                    event_sink.as_ref(),
                    &materializer,
                )
                .await;
                // Route the completion back to the orchestrator ingress via
                // the injected callback (wired in spur-core to avoid a
                // circular dependency). The delegation_id is used as a
                // worker_session proxy for the DelegationCompleted UI event.
                (ctx.on_complete)(cont, delegation_id.clone().into()).await;
            }
        });
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

    /// Test-only: invoke the `delegate_to_worker` JSON-RPC handler directly.
    ///
    /// Exposed solely so integration tests in sibling crates (e.g.
    /// `spur-core/tests/continuation_integration.rs`) can exercise the
    /// block-timeout / detached-completion paths without standing up the full
    /// HTTP stack. Returns the raw JSON-RPC response as a `serde_json::Value`.
    #[doc(hidden)]
    pub async fn __test_call_delegate_to_worker(&self, agent: &str, task: &str) -> Value {
        let resp = self
            .handle_delegate_to_worker(Value::from(1), json!({ "agent": agent, "task": task }))
            .await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `cancel_delegation` JSON-RPC handler directly.
    ///
    /// Mirrors `__test_call_delegate_to_worker`: exposed solely so integration
    /// tests in sibling crates (e.g. `spur-core/tests/cancellation.rs`) can
    /// drive the INV-ASYNC-3 cancel path deterministically without standing
    /// up the full HTTP stack.
    #[doc(hidden)]
    pub async fn __test_call_cancel_delegation(&self, delegation_id: &str) -> Value {
        let resp = self
            .handle_cancel_delegation(Value::from(2), json!({ "delegation_id": delegation_id }))
            .await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `delegate_parallel` JSON-RPC handler directly.
    ///
    /// Exposed for `crates/spur-mcp/tests/parallel_response_shape.rs` to exercise
    /// per-task parallelization without standing up the full HTTP stack.
    /// Returns the raw JSON-RPC response as a `serde_json::Value`.
    #[doc(hidden)]
    pub async fn __test_call_delegate_parallel(&self, tasks: Vec<(&str, &str)>) -> Value {
        let task_array: Value = Value::Array(
            tasks
                .iter()
                .enumerate()
                .map(|(idx, (agent, task))| {
                    json!({
                        "agent": agent,
                        "task": task,
                        "issue_id": format!("test-issue-{}", idx),
                    })
                })
                .collect(),
        );
        let args = json!({ "tasks": task_array });
        let resp = self.handle_delegate_parallel(Value::from(1), args).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `execute_epic` JSON-RPC handler directly.
    ///
    /// Exposed for integration tests that need to verify persisted label and
    /// audit behavior without standing up the full HTTP transport.
    #[doc(hidden)]
    pub async fn __test_call_execute_epic(
        &self,
        epic_id: &str,
        default_agent: Option<&str>,
    ) -> Value {
        let args = match default_agent {
            Some(agent) => json!({
                "epic_id": epic_id,
                "default_agent": agent,
            }),
            None => json!({
                "epic_id": epic_id,
            }),
        };
        let resp = self.handle_execute_epic(Value::from(1), args).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: invoke the `submit_plan` JSON-RPC handler directly.
    ///
    /// Accepts a raw `arguments` object so integration tests can exercise both
    /// ephemeral and persisted submit paths without the HTTP transport.
    #[doc(hidden)]
    pub async fn __test_call_submit_plan(&self, arguments: Value) -> Value {
        let resp = self.handle_submit_plan(Value::from(1), arguments).await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_install_startup_recovery_probe(
        probe: Arc<StartupRecoveryProbe>,
    ) -> StartupRecoveryProbeGuard {
        *STARTUP_RECOVERY_PROBE.lock().unwrap() = Some(probe);
        StartupRecoveryProbeGuard
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_request_startup_recovery(&self) {
        self.request_startup_recovery();
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn __test_drop_startup_recovery_handle(&self) {
        let handle = self.startup_recovery.lock().unwrap().handle.take();
        drop(handle);
    }

    /// Test-only: trigger the persisted-plan recovery path directly.
    #[doc(hidden)]
    pub async fn __test_recover_persisted_plans(&self) -> Result<(), String> {
        let Some(pm) = self.pm_service.clone() else {
            return Err("pm_service not configured".to_string());
        };
        self.recover_persisted_plans(pm)
            .await
            .map_err(|error| error.to_string())
    }

    /// Test-only: invoke any tool handler through the same JSON-RPC dispatch
    /// path used by the MCP transport.
    #[doc(hidden)]
    pub async fn __test_call_tool(&self, tool_name: &str, arguments: Value) -> Value {
        let response = self
            .handle_tool_call(
                Value::Null,
                json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await;
        serde_json::to_value(&response).expect("serialize JsonRpcResponse")
    }

    /// Test-only: install a plan state directly into the in-memory cache.
    #[doc(hidden)]
    pub async fn __test_install_plan(&self, state: crate::plan::PlanState) {
        self.install_projected_plan(state, false).await;
    }

    /// Test-only: mutate a cached plan entry into an impossible state so
    /// persisted read paths can prove they refresh from durable projection
    /// instead of trusting `active_plans`.
    #[doc(hidden)]
    pub async fn __test_corrupt_cached_plan(
        &self,
        plan_id: &str,
        task_id: &str,
        worker_branch: &str,
        base_snapshot_branch: &str,
    ) -> Result<(), String> {
        let plan = self
            .active_plans
            .lock()
            .await
            .get(plan_id)
            .cloned()
            .ok_or_else(|| format!("unknown cached plan '{plan_id}'"))?;
        let mut state = plan.lock().await;
        let entry = state
            .tasks
            .iter_mut()
            .find(|task| task.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in cached plan '{plan_id}'"))?;
        entry.status = crate::plan::PlanTaskStatus::Approved {
            summary: Some("corrupted-cache".into()),
        };
        entry.worker_branch = Some(worker_branch.to_string());
        state.base_snapshot_branch = Some(base_snapshot_branch.to_string());
        state.base_snapshot_oid = Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into());
        state.merge_state = crate::plan::PlanMergeState::Succeeded {
            merge_branch: "spur/bogus-merge".into(),
            merged_task_ids: vec![task_id.to_string()],
        };
        Ok(())
    }

    /// Test-only: peek whether a result is sitting in `completed_delegations`
    /// awaiting a `check_delegation_status` poll. Used to detect the
    /// double-delivery failure mode (map write AND continuation callback both
    /// firing for the same delegation).
    #[doc(hidden)]
    pub async fn __test_completed_has(&self, delegation_id: &str) -> bool {
        self.completed_delegations
            .lock()
            .await
            .contains_key(&DelegationId::from(delegation_id))
    }

    /// Test-only: current number of cached plan entries in `active_plans`.
    #[doc(hidden)]
    pub async fn __test_active_plan_count(&self) -> usize {
        self.active_plans.lock().await.len()
    }

    /// Test-only: report whether the reconciler task has been spawned.
    #[doc(hidden)]
    pub fn __test_reconciler_running(&self) -> bool {
        self.reconciler_handle.lock().unwrap().is_some()
    }

    /// Test-only: report whether legacy startup recovery has been requested
    /// but is waiting for a bound brain_session_id.
    #[doc(hidden)]
    pub fn __test_startup_recovery_pending(&self) -> bool {
        self.startup_recovery.lock().unwrap().pending
    }

    /// Test-only: wait for the spawned startup recovery task to complete.
    #[doc(hidden)]
    pub async fn __test_wait_startup_recovery(&self) {
        let handle = self.startup_recovery.lock().unwrap().handle.take();
        if let Some(handle) = handle {
            handle.wait().await;
        }
    }

    /// Remove completed delegation results older than `COMPLETED_TTL`.
    /// Called lazily from polling handlers to bound memory growth.
    async fn evict_stale_completions(&self) {
        self.completed_delegations
            .lock()
            .await
            .retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
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

        let Some(pm) = self.pm_service.as_ref() else {
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
        let pm = Arc::clone(pm);
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut reconciler = Reconciler::new(
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
                    let watcher = SignalWatcher::new(
                        pm,
                        ScopeDriftSplitProposer::default(),
                        TrivialScorer,
                        feature_gate,
                    );
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

    fn rmcp_tools(&self) -> Vec<Tool> {
        tools::tools_list()
            .into_iter()
            .map(|def| Tool::new(def.name, def.description, rmcp_object(def.input_schema)))
            .collect()
    }

    fn rmcp_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tools().into_iter().find(|tool| tool.name == name)
    }

    fn call_tool_result_from_legacy_response(
        response: JsonRpcResponse,
        tool_name: &str,
    ) -> Result<CallToolResult, McpError> {
        match (response.result, response.error) {
            (Some(result), None) => serde_json::from_value(result).map_err(|error| {
                McpError::internal_error(
                    format!("failed to serialize tool result for {tool_name}: {error}"),
                    None,
                )
            }),
            (None, Some(error)) => Err(error.into_mcp_error()),
            (Some(_), Some(_)) | (None, None) => Err(McpError::internal_error(
                format!("tool handler returned an invalid response envelope for {tool_name}"),
                None,
            )),
        }
    }

    // ─── Tool call dispatcher ─────────────────────────────────────────

    async fn handle_tool_call(&self, id: Value, params: Value) -> JsonRpcResponse {
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing 'name' in params"),
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        debug!(tool = %tool_name, "Handling tool call");

        if tokio::time::timeout(BRAIN_SESSION_BIND_TIMEOUT, self.brain_session_id_ready())
            .await
            .is_err()
        {
            return JsonRpcResponse::internal_error(id, "server not yet bound to brain session");
        }

        match tool_name.as_str() {
            "delegate_to_worker" => self.handle_delegate_to_worker(id, arguments).await,
            "delegate_parallel" => self.handle_delegate_parallel(id, arguments).await,
            "check_delegation_status" => self.handle_check_delegation_status(id, arguments).await,
            "fetch_outcome_artifact" => self.handle_fetch_outcome_artifact(id, arguments).await,
            "cancel_delegation" => self.handle_cancel_delegation(id, arguments).await,
            "list_available_workers" => self.handle_list_available_workers(id).await,
            "get_issue" => self.handle_get_issue(id, arguments).await,
            "list_issues" => self.handle_list_issues(id, arguments).await,
            "update_issue" => self.handle_update_issue(id, arguments).await,
            "report_signal" => {
                if let Some(response) =
                    self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
                {
                    return response;
                }
                let pm = match self.pm_service.clone() {
                    Some(pm) => pm,
                    None => {
                        return JsonRpcResponse::internal_error(id, "No issue tracker configured");
                    }
                };

                let ctx = crate::handlers::WorkerCallContext {
                    delegation_id: String::new(),
                    brain_session_id: self.brain_session_id().as_session_id().0.clone(),
                };
                match crate::handlers::report_signal(
                    pm.as_ref(),
                    self.feature_gate.as_ref(),
                    &ctx,
                    arguments,
                )
                .await
                {
                    Ok(result) => {
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        JsonRpcResponse::success(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                        JsonRpcResponse::invalid_params(id, e)
                    }
                    Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                        JsonRpcResponse::error(id, -32004, e)
                    }
                    Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                        JsonRpcResponse::error(id, -32001, e)
                    }
                    Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                        JsonRpcResponse::internal_error(id, format!("report_signal failed: {e}"))
                    }
                    Err(crate::handlers::McpHandlerError::Internal(e)) => {
                        JsonRpcResponse::internal_error(id, e)
                    }
                }
            }
            "create_issue" => self.handle_create_issue(id, arguments).await,
            "add_dependency" => self.handle_add_dependency(id, arguments).await,
            "create_pr" => self.handle_create_pr(id, arguments).await,
            "merge_plan" => self.handle_merge_plan(id, arguments).await,
            "resume_plan" => self.handle_resume_plan(id, arguments).await,
            "force_reclaim_plan" => self.handle_force_reclaim_plan(id, arguments).await,
            "graph_triage" => self.handle_graph_triage(id, arguments).await,
            "graph_plan" => self.handle_graph_plan(id, arguments).await,
            "graph_insights" => self.handle_graph_insights(id, arguments).await,
            "graph_alerts" => self.handle_graph_alerts(id, arguments).await,
            "graph_subgraph" => self.handle_graph_subgraph(id, arguments).await,
            "submit_plan" => self.handle_submit_plan(id, arguments).await,
            "execute_epic" => self.handle_execute_epic(id, arguments).await,
            "get_plan_status" => self.handle_get_plan_status(id, arguments).await,
            "get_reconciler_status" => self.handle_get_reconciler_status(id).await,
            "get_task_diff" => self.handle_get_task_diff(id, arguments).await,
            "preview_task_base" => self.handle_preview_task_base(id, arguments).await,
            "review_task" => {
                if let Some(plan_id) = arguments.get("plan_id").and_then(|v| v.as_str()) {
                    if let Err((code, message)) =
                        self.check_plan_owner_for_op(plan_id, "review_task").await
                    {
                        return JsonRpcResponse::error(id, code, message);
                    }
                }
                match self.handle_review_task(&arguments).await {
                    Ok(text) => JsonRpcResponse::success(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    ),
                    Err(e) => JsonRpcResponse::internal_error(id, e),
                }
            }
            "report_progress" => self.handle_report_progress(id, arguments).await,
            _ => JsonRpcResponse::error(id, -32601, format!("Unknown tool: {tool_name}")),
        }
    }

    // ─── Tool handlers ────────────────────────────────────────────────

    async fn handle_delegate_to_worker(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            match serde_json::from_value(args.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::invalid_params(id, format!("Invalid arguments: {e}"))
                }
            };

        let request_id = DelegationId::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let attempt_tracker = new_attempt_tracker();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: parsed.agent.clone(),
            task: parsed.task,
            context_files: parsed.context_files.unwrap_or_default(),
            respond_to: tx,
            brain_session_id: self.brain_session_id().clone(),
            delegation_plan: parsed.delegation_plan,
            issue_id: parsed.issue_id,
            base: parsed.base,
            dispatched_base_oid_tx: None,
            attempt_tracker: Arc::clone(&attempt_tracker),
            enable_worker_mcp: parsed.enable_worker_mcp,
        };

        info!(agent = %parsed.agent, request_id = %request_id, "Sending delegation request");

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            error!("Failed to send delegation request");
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        self.active_delegations
            .lock()
            .await
            .insert(request_id.clone());

        // Phase 1c (INV-ASYNC-1/2/3/7): biased select! over
        //   (fast arm: &mut rx), (slow arm: sleep(inline_wait))
        // Fast arm → return inline completion, drain active_delegations, no
        // collector spawn, no map write.
        // Slow arm → hand the receiver to `spawn_result_collector` with a
        // BlockTimeout continuation handle; the bridge is the sole delivery
        // channel (collector skips the map write when `detached` is Some).
        //
        // Cancel-during-handoff (Risk R2): the select! arm atomically either
        // consumes the oneshot result (fast path) or hands off the receiver
        // to the collector (slow path) — never both. `handle_cancel_delegation`
        // routes through `CancellationControl`, which signals the orchestrator
        // rather than touching our oneshot, so a cancel arriving between the
        // inline-window tick and the collector spawn races against the
        // orchestrator's own cancellation drain, not against this handler.
        //
        // INV-ASYNC-7: no mutex guards are held across any `.await` point
        // inside the arms below — `active_delegations.lock()` is scoped to a
        // single `.remove()` call in the fast arm.
        let mut rx = rx;
        let inline_wait = self.inline_wait;
        tokio::select! {
            biased;
            res = &mut rx => {
                let result = match res {
                    Ok(r) => r,
                    Err(_) => DelegationResult {
                        status: DelegationStatus::Failed {
                            error: "Orchestrator disconnected".into(),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                };
                self.active_delegations
                    .lock()
                    .await
                    .remove(&request_id);
                let result_json = match serde_json::to_value(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("Failed to serialize result: {e}"),
                        )
                    }
                };
                // Response shape (spec §8.3, post-review): content[0].text is
                // PURE JSON so brains can `json.loads(text)` without stripping
                // a leading shadow sentence. Human-readable context lives in
                // the `description` field.
                let payload = json!({
                    "status": "completed",
                    "delegation_id": request_id,
                    "continuation_will_fire": false,
                    "description": format!(
                        "Delegation to '{agent}' completed inline (delegation_id={request_id}).",
                        agent = parsed.agent
                    ),
                    "result": result_json,
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
            _ = tokio::time::sleep(inline_wait) => {
                info!(
                    agent = %parsed.agent,
                    request_id = %request_id,
                    inline_wait_ms = inline_wait.as_millis() as u64,
                    "Delegation inline window expired — detaching via continuation bridge"
                );
                Self::spawn_result_collector(
                    &self.task_tracker,
                    request_id.clone(),
                    rx,
                    self.cancel_token.child_token(),
                    Arc::clone(&self.active_delegations),
                    Arc::clone(&self.completed_delegations),
                    Some(DetachedCompletionHandle {
                        ctx: Arc::clone(&self.continuation_ctx),
                        source_kind: DetachedSourceKind::BlockTimeout,
                        attempt_tracker,
                        brain_session: self.brain_session_id().as_session_id().clone(),
                        event_sink: self.event_sink.clone(),
                        materializer: self.materializer.clone(),
                    }),
                );
                let payload = json!({
                    "status": "pending",
                    "delegation_id": request_id,
                    "continuation_will_fire": true,
                    "description": format!(
                        "Delegation to '{agent}' is running in the background \
                         (delegation_id={request_id}). A continuation event will \
                         fire automatically when the worker completes. Do NOT call \
                         check_delegation_status — you will be re-prompted automatically.",
                        agent = parsed.agent
                    ),
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
        }
    }

    async fn handle_delegate_parallel(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        if let Some(batch_plan) = args.get("delegation_plan") {
            tracing::info!(
                batch_plan = %batch_plan,
                "delegate_parallel received batch-level delegation_plan (not propagated into per-task requests)",
            );
        }

        if let Err(e) = validate_parallel_args(&args) {
            return JsonRpcResponse::invalid_params(id, e);
        }

        let skeletons = match parse_parallel_tasks(&args, self.brain_session_id()) {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        // Phase 2 (INV-ASYNC-6): split the batch into
        //   (1) dispatch: send every delegation request up front and capture
        //       `(idx, request_id, agent, rx)` for later waiting
        //   (2) concurrent await: run one biased `select!` per task in a
        //       `JoinSet`
        //   (3) aggregation: place each `(idx, Value)` back into a fixed
        //       response vector so the output order matches the input order.
        //
        // This preserves the single-worker fast/slow-arm semantics while
        // removing the Phase 1c serial-dispatch regression where task N+1
        // could not even be sent until task N finished its inline wait.
        let inline_wait = self.inline_wait;
        let task_count = skeletons.len();
        let mut dispatched = Vec::with_capacity(task_count);

        for (idx, mut skeleton) in skeletons.into_iter().enumerate() {
            let request_id = skeleton.id.clone();
            let agent = skeleton.agent.clone();
            let attempt_tracker = Arc::clone(&skeleton.attempt_tracker);
            let (tx, rx) = tokio::sync::oneshot::channel();
            skeleton.respond_to = tx;

            info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if let Err(_e) = self.delegation_tx.send(skeleton).await {
                error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.active_delegations
                .lock()
                .await
                .insert(request_id.clone());
            dispatched.push((idx, request_id, agent, rx, attempt_tracker));
        }

        let mut waits = JoinSet::new();
        for (idx, request_id, agent, rx, attempt_tracker) in dispatched {
            let active_delegations = Arc::clone(&self.active_delegations);
            let completed_delegations = Arc::clone(&self.completed_delegations);
            let continuation_ctx = Arc::clone(&self.continuation_ctx);
            let task_tracker = self.task_tracker.clone();
            let cancel_token = self.cancel_token.child_token();
            let event_sink = self.event_sink.clone();
            let brain_session = self.brain_session_id().as_session_id().clone();
            let materializer = self.materializer.clone();
            waits.spawn(async move {
                let mut rx = rx;
                // Cancel-during-handoff (Risk R2): see
                // `handle_delegate_to_worker` — the select! arm atomically
                // either consumes the result or hands off the receiver; it
                // does not do both.
                let entry = tokio::select! {
                    biased;
                    res = &mut rx => {
                        let result = match res {
                            Ok(r) => r,
                            Err(_) => DelegationResult {
                                status: DelegationStatus::Failed {
                                    error: "Orchestrator disconnected".into(),
                                },
                                diff: None,
                                diff_summary: None,
                                summary: None,
                                estimated_cost_usd: 0.0,
                                worker_branch: None,
                                artifact: None,
                            },
                        };
                        active_delegations
                            .lock()
                            .await
                            .remove(&request_id);
                        let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
                        json!({
                            "status": "completed",
                            "delegation_id": request_id,
                            "agent": agent,
                            "continuation_will_fire": false,
                            "description": format!(
                                "Delegation to '{agent}' completed inline (delegation_id={request_id})."
                            ),
                            "result": result_json,
                        })
                    }
                    _ = tokio::time::sleep(inline_wait) => {
                        Self::spawn_result_collector(
                            &task_tracker,
                            request_id.clone(),
                            rx,
                            cancel_token,
                            active_delegations,
                            completed_delegations,
                            Some(DetachedCompletionHandle {
                                ctx: continuation_ctx,
                                source_kind: DetachedSourceKind::BlockTimeout,
                                attempt_tracker,
                                brain_session,
                                event_sink,
                                materializer,
                            }),
                        );
                        json!({
                            "status": "pending",
                            "delegation_id": request_id,
                            "agent": agent,
                            "continuation_will_fire": true,
                            "description": format!(
                                "Delegation to '{agent}' is running in the background \
                                 (delegation_id={request_id}). A continuation event will \
                                 fire automatically when the worker completes. Do NOT call \
                                 check_delegation_status — you will be re-prompted automatically."
                            ),
                        })
                    }
                };
                (idx, entry)
            });
        }

        let mut results = vec![Value::Null; task_count];
        while let Some(join_result) = waits.join_next().await {
            let (idx, entry) = match join_result {
                Ok(result) => result,
                Err(e) => {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("Parallel delegation waiter failed: {e}"),
                    )
                }
            };
            results[idx] = entry;
        }

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&Value::Array(results.clone()))
                        .unwrap_or_else(|_| Value::Array(results).to_string())
                }]
            }),
        )
    }

    async fn handle_check_delegation_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        self.evict_stale_completions().await;

        // Completed — return and remove.
        let completed = {
            let mut map = self.completed_delegations.lock().await;
            map.retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
            map.remove(&delegation_id).map(|(r, _)| r)
        };
        if let Some(result) = completed {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Still running.
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": json!({"status": "running", "delegation_id": delegation_id}).to_string()
                    }]
                }),
            );
        }

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
    }

    async fn handle_fetch_outcome_artifact(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::fetch_outcome_artifact(
            &self.materializer,
            self.outcome_store.as_ref(),
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => JsonRpcResponse::success(id, value),
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("fetch_outcome_artifact failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_cancel_delegation(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        // Already completed — return the result directly.
        if let Some((result, _ts)) = self
            .completed_delegations
            .lock()
            .await
            .remove(&delegation_id)
        {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Active — use the CancellationControl side-channel (INV-6).
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            if let Some(ref cc) = self.cancellation_control {
                let outcome = cc
                    .cancel_with_reason(delegation_id.as_str(), "brain requested cancel".into())
                    .await;
                info!(delegation_id = %delegation_id, ?outcome, "Cancellation requested via CancellationControl");
                match outcome {
                    CancelOutcome::Cancelled => {
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} cancelled", delegation_id)
                                }]
                            }),
                        );
                    }
                    CancelOutcome::NotFound => {
                        // Token was already removed (delegation completed between
                        // the active_delegations check and the cancel call).
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} already completed", delegation_id)
                                }]
                            }),
                        );
                    }
                }
            } else {
                return JsonRpcResponse::internal_error(
                    id,
                    "cancel_delegation: no cancellation control wired",
                );
            }
        }

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
    }

    async fn handle_list_available_workers(&self, id: Value) -> JsonRpcResponse {
        let workers_json = serde_json::to_value(&self.workers).unwrap_or(json!([]));
        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&workers_json)
                        .unwrap_or_else(|_| workers_json.to_string())
                }]
            }),
        )
    }

    async fn handle_get_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };

        match crate::handlers::get_issue(pm, &ctx, args).await {
            Ok(issue) => {
                let text =
                    serde_json::to_string_pretty(&issue).unwrap_or_else(|_| issue.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("get_issue failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_list_issues(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let filter = IssueFilter {
            status: args
                .get("status")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            priority_min: args
                .get("priority_min")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            priority_max: args
                .get("priority_max")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            issue_type: args
                .get("issue_type")
                .and_then(|v| v.as_str())
                .map(String::from),
            text_search: args
                .get("text_search")
                .and_then(|v| v.as_str())
                .map(String::from),
            limit: Some(
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .min(100) as usize,
            ),
            offset: None,
            labels,
            since: None,
            include_closed: false,
        };

        match pm.list_issues(filter).await {
            Ok(issues) => {
                let text =
                    serde_json::to_string_pretty(&issues).unwrap_or_else(|_| format!("{issues:?}"));
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("list_issues failed: {e}")),
        }
    }

    async fn handle_update_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };

        match crate::handlers::update_issue(pm, &ctx, args).await {
            Ok(result) => {
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("update_issue failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_create_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let depends_on: Vec<String> = args
            .get("depends_on")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let params = spur_pm::IssueCreate {
            title,
            description: args
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            issue_type: args.get("type").and_then(|v| v.as_str()).map(String::from),
            priority: args
                .get("priority")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            labels,
            parent: args
                .get("parent")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            estimate_minutes: args
                .get("estimate")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            depends_on,
        };

        match pm.create_issue(params).await {
            Ok(issue_id) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Issue created: {issue_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_issue failed: {e}")),
        }
    }

    async fn handle_add_dependency(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let issue_id = match args.get("issue_id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(id, "Missing required field 'issue_id'")
            }
        };
        let depends_on_id = match args.get("depends_on_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'depends_on_id'",
                )
            }
        };

        match pm.add_dependency(&issue_id, &depends_on_id).await {
            Ok(()) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Dependency added: {issue_id} depends on {depends_on_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("add_dependency failed: {e}")),
        }
    }

    async fn handle_create_pr(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No PR service configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };
        let body = match args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'body'"),
        };
        let head_branch = match args.get("branch").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'branch'"),
        };

        let params = PrParams {
            title,
            body,
            head_branch,
            base_branch: args
                .get("base_branch")
                .and_then(|v| v.as_str())
                .map(String::from),
            repo: args.get("repo").and_then(|v| v.as_str()).map(String::from),
        };

        match pm.create_pr(params).await {
            Ok(url) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": format!("PR created: {url}") }] }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_pr failed: {e}")),
        }
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
                    pm,
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
        pm: &spur_pm::PmService,
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
        pm: &spur_pm::PmService,
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

    async fn handle_merge_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'plan_id'"),
        };

        if let Err((code, message)) = self.check_plan_owner_for_op(&plan_id, "merge_plan").await {
            return JsonRpcResponse::error(id, code, message);
        }

        match self.merge_plan_impl(&plan_id).await {
            Ok(merge_state) => {
                let plan_arc = match self.load_or_project_plan(&plan_id).await {
                    Ok(p) => p,
                    Err(e) => return JsonRpcResponse::invalid_params(id, e),
                };
                {
                    let mut state = plan_arc.lock().await;
                    state.merge_state = merge_state;
                }
                let state = plan_arc.lock().await;
                let status = crate::plan::build_plan_status(&plan_id, &state);
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not fully approved yet") || msg.contains("Unknown plan_id") {
                    JsonRpcResponse::invalid_params(id, msg)
                } else {
                    JsonRpcResponse::internal_error(id, msg)
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: invoke `resume_plan` and reduce the
    /// JSON-RPC response to a simple Result. Error message is verbatim from the
    /// MCP tool's `JsonRpcError.message`.
    pub async fn call_resume_plan(&self, plan_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "plan_id": plan_id });
        let resp = self
            .handle_resume_plan_with(serde_json::Value::Null, args, false)
            .await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    /// Public bridge for orchestrator/TUI: claim ownership for a persisted plan
    /// without starting dispatch. The pending gate keeps the reconciler from
    /// observing ready work until `call_resume_plan` explicitly starts it.
    pub async fn call_claim_plan(&self, plan_id: &str) -> Result<(), String> {
        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| "claim_plan requires PM service".to_string())?;

        let epic_summary =
            find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "claim_plan").await?;
        let epic_id = epic_summary.id.clone();
        let epic = pm
            .get_issue(&epic_id)
            .await
            .map_err(|error| format!("claim_plan: failed to load epic {epic_id}: {error}"))?;

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                if let Some(active) = self
                    .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                    .await?
                {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); cannot claim different active plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }
                Ok(())
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Err(format!(
                "claim_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
            )),
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => Err(format!(
                "claim_plan: plan {plan_id} has ambiguous owner labels: {}",
                owners.join(", ")
            )),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                if let Some(active) = self.current_brain_active_owned_plan(pm, None, None).await? {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }

                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![
                            owner_label.clone(),
                            crate::plan::labels::PLAN_PENDING.to_string(),
                        ],
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| format!("claim_plan: failed to claim plan: {error}"))?;

                let epic = pm.get_issue(&epic_id).await.map_err(|error| {
                    format!("claim_plan: failed to reload claimed epic {epic_id}: {error}")
                })?;
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Ok(()),
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                            owners.join(", ")
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => Err(format!(
                        "claim_plan: failed to claim plan {plan_id}; plan remains unowned"
                    )),
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: project a persisted plan and emit a
    /// `PlanSnapshotUpdated` without claiming ownership or changing plan state.
    pub async fn call_inspect_plan(&self, plan_id: &str) -> Result<(), String> {
        let plan = self.load_or_project_plan(plan_id).await?;
        let state = plan.lock().await;
        crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        Ok(())
    }

    async fn handle_resume_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_resume_plan_with(id, args, true).await
    }

    async fn handle_resume_plan_with(
        &self,
        id: Value,
        args: Value,
        allow_claim_unowned: bool,
    ) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|value| value.as_str()) {
            Some(plan_id) => plan_id,
            None => return JsonRpcResponse::invalid_params(id, "resume_plan: missing plan_id"),
        };
        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "resume_plan requires PM service"),
        };

        let epic_summary =
            match find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "resume_plan").await {
                Ok(epic) => epic,
                Err(error) => {
                    return if error.contains("plan not found") {
                        JsonRpcResponse::error(id, -32004, error)
                    } else if error.contains("ambiguous plan lookup") {
                        JsonRpcResponse::error(id, -32009, error)
                    } else {
                        JsonRpcResponse::internal_error(id, error)
                    }
                }
            };
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("resume_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                match self.is_projected_plan_nonterminal(plan_id).await {
                    Ok(true) => {
                        match self
                            .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                            .await
                        {
                            Ok(Some(active)) => {
                                return JsonRpcResponse::error(
                                    id,
                                    -32009,
                                    format!(
                                        "resume_plan: current brain session already owns active plan {} (epic {}); cannot resume different active plan {plan_id}",
                                        active.plan_id, active.epic_id
                                    ),
                                );
                            }
                            Ok(None) => {}
                            Err(error) => {
                                return JsonRpcResponse::internal_error(
                                    id,
                                    format!("resume_plan: {error}"),
                                )
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to project plan {plan_id}: {error}"),
                        )
                    }
                }
                let was_pending = epic
                    .labels
                    .iter()
                    .any(|label| label == crate::plan::labels::PLAN_PENDING);
                if was_pending {
                    if let Err(error) = apply_issue_update(
                        pm,
                        &epic_id,
                        IssueUpdate {
                            add_labels: vec![crate::plan::labels::PLAN_COMPLETE.to_string()],
                            remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to start plan: {error}"),
                        );
                    }
                }
                self.fast_forward_reconciler();
                let result = json!({
                    "status": if was_pending { "started" } else { "already_owner" },
                    "plan_id": plan_id,
                    "epic_id": epic_id,
                });
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "resume_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
                    ),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "resume_plan: plan {plan_id} has ambiguous owner labels: {}",
                    owners.join(", ")
                ),
            ),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                if !allow_claim_unowned {
                    return JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: plan {plan_id} is unowned; claim it before starting"),
                    );
                }
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                match self.current_brain_active_owned_plan(pm, None, None).await {
                    Ok(Some(active)) => {
                        return JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                                active.plan_id, active.epic_id
                            ),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: {error}"),
                        )
                    }
                }
                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                if let Err(error) = apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![owner_label.clone()],
                        ..Default::default()
                    },
                )
                .await
                {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("resume_plan: failed to claim plan: {error}"),
                    );
                }
                self.fast_forward_reconciler();

                let epic = match pm.get_issue(&epic_id).await {
                    Ok(epic) => epic,
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!(
                                "resume_plan: failed to reload claimed epic {epic_id}: {error}"
                            ),
                        )
                    }
                };
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                        let result = json!({
                            "status": "claimed",
                            "plan_id": plan_id,
                            "epic_id": epic_id,
                        });
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        JsonRpcResponse::success(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                                owners.join(", ")
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: failed to claim plan {plan_id}; plan remains unowned"),
                    ),
                }
            }
        }
    }

    /// Operator-initiated force-reclaim. Removes any existing
    /// `spur:plan-owner:*` labels from the plan's epic and stamps the current
    /// brain. Records a `PlanForceReclaimed` audit sentinel with the prior
    /// owner (or `None` if Unowned) and an optional operator-supplied reason.
    /// Refuses unless `confirm: true` is passed.
    async fn handle_force_reclaim_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(plan_id) => plan_id,
            None => {
                return JsonRpcResponse::invalid_params(id, "force_reclaim_plan: missing plan_id")
            }
        };
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirm {
            return JsonRpcResponse::invalid_params(
                id,
                "force_reclaim_plan: missing or false `confirm`. This tool clobbers any \
                 concurrent owner brain's in-flight state and is intended only for stuck or \
                 dead owners. Re-invoke with `confirm: true` to acknowledge.",
            );
        }
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "force_reclaim_plan requires PM service",
                )
            }
        };

        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }

        let epics = match pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await
        {
            Ok(epics) => epics,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to find plan: {error}"),
                )
            }
        };
        let Some(epic_summary) = epics.first() else {
            return JsonRpcResponse::error(
                id,
                -32004,
                format!("force_reclaim_plan: plan not found: {plan_id}"),
            );
        };
        if epics.len() > 1 {
            let epic_ids = epics
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "force_reclaim_plan: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
                ),
            );
        }
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        // Capture prior owner(s) for the audit sentinel BEFORE rewriting labels.
        // The single-owner case yields `Some("<owner>")`; the rare ambiguous
        // multi-owner case preserves the comma-joined list verbatim so operators
        // can see what was clobbered. Unowned → `None`.
        let prior_owners: Vec<String> = epic
            .labels
            .iter()
            .filter_map(|label| {
                crate::plan::labels::parse_plan_owner(label).map(|owner| owner.to_string())
            })
            .collect();
        let prior_owner = match prior_owners.len() {
            0 => None,
            1 => Some(prior_owners[0].clone()),
            _ => Some(prior_owners.join(",")),
        };

        let new_owner_label =
            crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
        let mut remove_labels: Vec<String> = epic
            .labels
            .iter()
            .filter(|label| crate::plan::labels::parse_plan_owner(label).is_some())
            .cloned()
            .collect();
        let add_labels = vec![new_owner_label.clone()];
        filter_remove_labels(&mut remove_labels, &add_labels);

        if let Err(error) = apply_issue_update(
            pm,
            &epic_id,
            IssueUpdate {
                add_labels,
                remove_labels,
                ..Default::default()
            },
        )
        .await
        {
            return JsonRpcResponse::internal_error(
                id,
                format!(
                    "force_reclaim_plan: failed to write owner labels on epic {epic_id}: {error}"
                ),
            );
        }
        self.fast_forward_reconciler();

        let new_owner = self.brain_session_id().to_string();
        let token = uuid::Uuid::new_v4().to_string();

        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Some(adv) = pm.advanced() {
            let audit = crate::plan::audit_sentinel::AuditSentinelKind::PlanForceReclaimed {
                plan_id: plan_id.to_string(),
                prior_owner: prior_owner.clone(),
                new_owner: new_owner.clone(),
                token: token.clone(),
                reason: reason.clone(),
            };
            let body = crate::plan::audit_sentinel::encode_comment(&audit);
            if let Err(e) = adv.add_comment(&epic_id, &body).await {
                tracing::warn!(
                    target: "spur.audit.emit_failure",
                    kind = "plan_force_reclaimed",
                    epic_id = %epic_id,
                    plan_id = %plan_id,
                    "PlanForceReclaimed audit comment emission failed (owner label is persisted; audit missing): {e}"
                );
            }
        }

        let result = json!({
            "prior_owner": prior_owner,
            "new_owner": new_owner,
            "audit_token": token,
        });
        let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    // ─── Graph analysis handlers (bv robot protocol) ───────────────

    /// Helper: get the bv analyzer or return an MCP error.
    #[allow(clippy::result_large_err)]
    fn require_analyzer(&self, id: &Value) -> Result<&spur_pm::BvAdapter, JsonRpcResponse> {
        let pm = self.pm_service.as_ref().ok_or_else(|| {
            JsonRpcResponse::internal_error(id.clone(), "No PM service configured")
        })?;
        pm.analyzer().ok_or_else(|| {
            JsonRpcResponse::internal_error(
                id.clone(),
                "Graph analysis not available (beads database unavailable)",
            )
        })
    }

    async fn handle_graph_triage(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.triage(label).await {
            Ok(report) => {
                let text = serde_json::to_string_pretty(&report.raw)
                    .unwrap_or_else(|_| report.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_triage failed: {e}")),
        }
    }

    async fn handle_graph_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.plan(label).await {
            Ok(plan) => {
                let text = serde_json::to_string_pretty(&plan.raw)
                    .unwrap_or_else(|_| plan.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_plan failed: {e}")),
        }
    }

    async fn handle_graph_insights(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.insights(label).await {
            Ok(insights) => {
                let text = serde_json::to_string_pretty(&insights.raw)
                    .unwrap_or_else(|_| insights.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_insights failed: {e}")),
        }
    }

    async fn handle_graph_alerts(&self, id: Value, _args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        match bv.alerts().await {
            Ok(report) => {
                let text = serde_json::to_string_pretty(&report.raw)
                    .unwrap_or_else(|_| report.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_alerts failed: {e}")),
        }
    }

    async fn handle_graph_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let root_id = match args.get("root_id").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'root_id'"),
        };
        let depth = args.get("depth").and_then(|v| v.as_u64()).map(|d| d as u32);
        let format = args.get("format").and_then(|v| v.as_str());
        match bv.subgraph(root_id, depth, format).await {
            Ok(graph) => {
                let text = serde_json::to_string_pretty(&graph.raw)
                    .unwrap_or_else(|_| graph.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_subgraph failed: {e}")),
        }
    }

    // ─── Plan execution handlers ──────────────────────────────────

    async fn handle_submit_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let tasks_val = match args.get("tasks").and_then(|v| v.as_array()) {
            Some(t) => t.clone(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'tasks'"),
        };

        let mut tasks: Vec<crate::plan::PlanTask> = match tasks_val
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(t) => t,
            Err(e) => {
                return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
            }
        };

        let auto_serialized = match crate::plan::submit_plan_normalize_tasks(&mut tasks) {
            Ok(overlaps) => overlaps,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        // ─── Persist-as-epic extraction (T2.1) ─────────────────────────
        let persist_as_epic = args
            .get("persist_as_epic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let epic_title = args
            .get("epic_title")
            .and_then(|v| v.as_str())
            .map(String::from);
        let epic_body = args
            .get("epic_body")
            .and_then(|v| v.as_str())
            .map(String::from);

        if persist_as_epic {
            if let Some(response) =
                self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
            {
                return response;
            }
            if epic_title
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                return JsonRpcResponse::invalid_params(
                    id,
                    "submit_plan: epic_title is required when persist_as_epic is true",
                );
            }
            let pm_source = self.pm_service.as_deref().map(|p| p.source_str());
            if pm_source != Some("beads") {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!(
                        "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                        pm_source.unwrap_or("none"),
                    ),
                );
            }
        }
        let plan_id = uuid::Uuid::new_v4().to_string();

        // Parse optional explicit base. Tolerant: `BaseTarget`'s manual
        // Deserialize accepts both `{"kind":...}` and JSON-stringified-object.
        let explicit_base: Option<crate::tools::BaseTarget> = match args.get("base") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match serde_json::from_value::<crate::tools::BaseTarget>(v.clone()) {
                Ok(target) => Some(target),
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan: invalid 'base' parameter: {e}"),
                    );
                }
            },
        };

        // Build the beads epic subgraph before spawning the executor so
        // any creation error is surfaced synchronously.
        let epic_subgraph: Option<EpicSubgraph> = if persist_as_epic {
            let pm = self
                .pm_service
                .as_deref()
                .expect("gate ensures pm is beads");
            let title = epic_title.as_deref().expect("gate ensures non-empty title");
            let owner_label =
                crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
            match build_epic_subgraph_with_activation_labels(
                pm,
                self.feature_gate.as_ref(),
                &plan_id,
                title,
                epic_body.as_deref(),
                &tasks,
                vec![owner_label],
            )
            .await
            {
                Ok(sg) => {
                    if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
                        return JsonRpcResponse::mcp_error(id.clone(), error);
                    }
                    if let Some(adv) = pm.advanced() {
                        let audit =
                            crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                                plan_id: plan_id.clone(),
                                owner: self.brain_session_id().to_string(),
                                token: uuid::Uuid::new_v4().to_string(),
                                reason: "submit_plan".to_string(),
                            };
                        let body = crate::plan::audit_sentinel::encode_comment(&audit);
                        if let Err(e) = adv.add_comment(&sg.epic_id, &body).await {
                            tracing::warn!(
                                target: "spur.audit.emit_failure",
                                kind = "plan_ownership_acquired",
                                epic_id = %sg.epic_id,
                                plan_id = %plan_id,
                                "PlanOwnershipAcquired audit comment emission failed (owner label is persisted; audit missing): {e}"
                            );
                        }
                    }

                    info!(
                        plan_id = %plan_id,
                        epic_id = %sg.epic_id,
                        children = sg.task_map.len(),
                        "submit_plan: beads epic subgraph created"
                    );
                    Some(sg)
                }
                Err(e) => {
                    error!(plan_id = %plan_id, "build_epic_subgraph failed: {e}");
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("submit_plan: failed to persist plan as beads epic: {e}"),
                    );
                }
            }
        } else {
            None
        };

        let entries: Vec<crate::plan::PlanTaskEntry> =
            build_entries_with_task_map(tasks, epic_subgraph.as_ref().map(|sg| &sg.task_map));

        let task_count = entries.len();
        let base_snapshot =
            match resolve_plan_base(self.repo_root.as_ref(), explicit_base.as_ref()).await {
                Ok(snapshot) => snapshot,
                Err(e) => return JsonRpcResponse::internal_error(id, e),
            };
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id().clone(),
            base_snapshot_branch: base_snapshot.branch,
            base_snapshot_oid: base_snapshot.oid,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: epic_subgraph.as_ref().map(|sg| sg.epic_id.clone()),
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        if let Some(sg) = &epic_subgraph {
            if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
                return JsonRpcResponse::mcp_error(id, error);
            }
            if let Some(adv) = self.pm_service.as_deref().and_then(|pm| pm.advanced()) {
                let (base_snapshot_branch, base_snapshot_oid) = {
                    let state = state.lock().await;
                    (
                        state.base_snapshot_branch.clone(),
                        state.base_snapshot_oid.clone(),
                    )
                };
                emit_plan_submit_audit(
                    adv,
                    &plan_id,
                    sg,
                    base_snapshot_branch.as_deref(),
                    base_snapshot_oid.as_deref(),
                    Some("submit_plan"),
                    Some(self.brain_session_id().as_session_id()),
                    explicit_base.as_ref(),
                )
                .await;
            }
        }

        self.active_plans
            .lock()
            .await
            .insert(plan_id.clone(), Arc::clone(&state));

        if epic_subgraph.is_some() {
            let state = state.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        }

        if epic_subgraph.is_some() {
            self.fast_forward_reconciler();
        } else {
            self.spawn_ephemeral_plan_runner(state);
        }

        info!(plan_id = %plan_id, tasks = task_count, "Plan submitted");

        let response_text = if let Some(sg) = &epic_subgraph {
            let task_map_json =
                serde_json::to_string(&sg.task_map).unwrap_or_else(|_| "{}".to_string());
            format!(
                "Plan submitted: {task_count} tasks.\n\
                 plan_id: {plan_id}\n\
                 epic_id: {epic_id} (beads)\n\
                 task_map: {task_map_json}\n\
                 A continuation will fire on each per-task failure/awaiting-review and on plan completion. \
                 Polling get_plan_status remains available as a safety net.",
                epic_id = sg.epic_id,
            )
        } else {
            format!(
                "Plan submitted: {task_count} tasks. plan_id: {plan_id}\n\
                 A continuation will fire on each per-task failure/awaiting-review and on plan completion. \
                 Polling get_plan_status remains available as a safety net."
            )
        };

        let response_text = if auto_serialized.is_empty() {
            response_text
        } else {
            let edges: Vec<String> = auto_serialized
                .iter()
                .map(|o| {
                    format!(
                        "  {} → {} (shared: {})",
                        o.from,
                        o.to,
                        o.shared_files.join(", ")
                    )
                })
                .collect();
            format!(
                "{response_text}\n\nAuto-serialized {} sibling pair(s) with overlapping context_files:\n{}",
                auto_serialized.len(),
                edges.join("\n")
            )
        };

        JsonRpcResponse::success(
            id,
            json!({
                "continuation_will_fire": true,
                "auto_serialized": auto_serialized,
                "content": [{
                    "type": "text",
                    "text": response_text
                }]
            }),
        )
    }

    async fn handle_execute_epic(&self, id: Value, args: Value) -> JsonRpcResponse {
        // 1. Extract required epic_id.
        let epic_id = match args.get("epic_id").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "missing required field: epic_id"),
        };
        let default_agent = args
            .get("default_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // 2. Require PmService.
        // Unit-tested via integration-level fixtures only; the PmService gate is
        // the first check in handle_execute_epic and its error message matches
        // this literal: "beads (PmService) is not configured — cannot execute epic".
        let pm = match self.pm_service.as_deref() {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    "beads (PmService) is not configured — cannot execute epic",
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }

        let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;

        // Owner-classification gate. Refuses takeover (OwnedByOther) and
        // ambiguous owner labels before reserving the registry slot. Unowned
        // (claim) and OwnedByCurrent (re-issue) proceed. The
        // PlanOwnershipTransferred audit branch downstream stays intact as
        // defense-in-depth for a future force-reclaim path.
        // Fail fast on PM fetch error so a transient beads outage cannot
        // silently bypass the gate (mirrors check_plan_owner_for_op).
        let epic_issue = match pm.get_issue(&epic_id).await {
            Ok(issue) => issue,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32603,
                    format!("execute_epic: failed to load epic {epic_id}: {error}"),
                );
            }
        };
        if let Some(existing_plan_id) = persisted_plan_epic_plan_id(&epic_issue) {
            return JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "execute_epic: epic {epic_id} is already a persisted plan epic for plan {existing_plan_id}; use claim/start/resume plan instead"
                ),
            );
        }
        match crate::plan::ownership::classify_owner(
            &epic_issue.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::Unowned
            | crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: epic {epic_id} is owned by {owner}; active handoff is not implemented in MVP"
                    ),
                );
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: epic {epic_id} has ambiguous owner labels: {}",
                        owners.join(", ")
                    ),
                );
            }
        }

        // Sentinel value used to reserve a registry slot while the PmService
        // fetch is in flight. Concurrent callers that see this value return an
        // "already in progress" error instead of racing into double-dispatch.
        const PENDING_SENTINEL: &str = "__pending__";

        // 3. Idempotency + reservation: under a single lock acquisition,
        //    either return the existing non-terminal plan, reserve the slot
        //    with a sentinel (and fall through to the fetch), or clear a
        //    stale/terminal entry and reserve.
        {
            let mut registry = self.plan_registry.lock().await;
            match registry.by_epic.get(&epic_id).cloned() {
                Some(ref existing) if existing == PENDING_SENTINEL => {
                    // A concurrent call is already in the fetch/derive phase.
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!(
                            "execute_epic for epic '{epic_id}' is already in progress — \
                             wait for it to complete and call get_plan_status"
                        ),
                    );
                }
                Some(existing_plan_id) => {
                    // Check if the existing plan is still non-terminal.
                    // Persisted plans must be reprojected here so stale cache
                    // state cannot block a legitimate rerun.
                    drop(registry);
                    let plan_arc = self.load_or_project_plan(&existing_plan_id).await.ok();
                    if let Some(arc) = plan_arc {
                        let state = arc.lock().await;
                        let status_val = crate::plan::build_plan_status(&existing_plan_id, &state);
                        let overall = status_val
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        if !crate::plan::is_terminal_plan_status(overall) {
                            // Return existing plan status.
                            let mut resp_val = status_val;
                            if let serde_json::Value::Object(ref mut m) = resp_val {
                                m.insert("epic_id".into(), serde_json::json!(epic_id));
                                m.insert(
                                    "next_action".into(),
                                    serde_json::json!(
                                        "Plan already active for this epic. \
                                         Poll with get_plan_status(plan_id) to monitor progress."
                                    ),
                                );
                            }
                            let text = serde_json::to_string_pretty(&resp_val)
                                .unwrap_or_else(|_| resp_val.to_string());
                            return JsonRpcResponse::success(
                                id,
                                json!({ "content": [{ "type": "text", "text": text }] }),
                            );
                        }
                        // Terminal plan — fall through to start a fresh one.
                        // Re-acquire the registry lock to insert the sentinel.
                    }
                    // Plan not found in active_plans (evicted or never inserted)
                    // or was terminal — reserve the slot now.
                    self.plan_registry
                        .lock()
                        .await
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
                None => {
                    // No entry at all — reserve the slot.
                    registry
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
            }
        }

        match self.nonterminal_plan_status_for_epic(pm, &epic_id).await {
            Ok(Some((existing_plan_id, status_val))) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                self.plan_registry
                    .lock()
                    .await
                    .by_epic
                    .insert(epic_id.clone(), existing_plan_id);

                let mut resp_val = status_val;
                if let serde_json::Value::Object(ref mut m) = resp_val {
                    m.insert("epic_id".into(), serde_json::json!(epic_id));
                    m.insert(
                        "next_action".into(),
                        serde_json::json!(
                            "Plan already active for this epic. \
                             Poll with get_plan_status(plan_id) to monitor progress."
                        ),
                    );
                }
                let text = serde_json::to_string_pretty(&resp_val)
                    .unwrap_or_else(|_| resp_val.to_string());
                return JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, format!("execute_epic: {error}"));
            }
        }

        match self
            .current_brain_active_owned_plan(pm, None, Some(&epic_id))
            .await
        {
            Ok(Some(active)) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: current brain session already owns active plan {} (epic {}); finish it before executing epic {epic_id}",
                        active.plan_id, active.epic_id
                    ),
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, format!("execute_epic: {error}"));
            }
        }

        // 4. Derive the plan from the epic subgraph via PmService.
        let known_agent_names: Vec<String> = self.workers.iter().map(|w| w.name.clone()).collect();
        let known_agents_refs: Vec<&str> = known_agent_names.iter().map(String::as_str).collect();

        let derived = match crate::plan::derive_epic_plan(
            pm,
            self.feature_gate.as_ref(),
            &epic_id,
            default_agent.as_deref(),
            &known_agents_refs,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                // Clear the sentinel so callers can retry after fixing the issue.
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::error(id, -32000, e);
            }
        };

        // 5. Build PlanState and spawn the plan — mirrors handle_submit_plan exactly.
        let plan_id = uuid::Uuid::new_v4().to_string();
        let entries: Vec<crate::plan::PlanTaskEntry> = derived
            .plan_tasks
            .into_iter()
            .map(|spec| crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            })
            .collect();

        let task_count = entries.len();
        let base_snapshot = match resolve_plan_base(self.repo_root.as_ref(), None).await {
            Ok(snapshot) => snapshot,
            Err(e) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, e);
            }
        };
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id().clone(),
            base_snapshot_branch: base_snapshot.branch,
            base_snapshot_oid: base_snapshot.oid,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some(epic_id.clone()),
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        // Keep a clone of the Arc to build the initial status response.
        let state_for_status = Arc::clone(&state);

        let (task_scope, base_snapshot_branch, base_snapshot_oid) = {
            let state = state_for_status.lock().await;
            let task_scope = state
                .tasks
                .iter()
                .filter_map(|entry| {
                    entry.spec.issue_id.as_ref().map(|issue_id| {
                        (
                            issue_id.clone(),
                            entry.spec.task_id.clone(),
                            entry.spec.agent.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            (
                task_scope,
                state.base_snapshot_branch.clone(),
                state.base_snapshot_oid.clone(),
            )
        };

        let mut rollback_updates: Vec<(String, spur_pm::IssueUpdate)> = Vec::new();
        let mut prior_owner_match: Option<crate::plan::ownership::PlanOwnerMatch> = None;
        if let Ok(epic_issue) = pm.get_issue(&epic_id).await {
            prior_owner_match = Some(crate::plan::ownership::classify_owner(
                &epic_issue.labels,
                self.brain_session_id().as_session_id(),
            ));
            let mut remove_labels = Vec::new();
            let owner_label =
                crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
            for label in &epic_issue.labels {
                if crate::plan::labels::parse_plan_id(label).is_some()
                    || crate::plan::labels::parse_agent(label).is_some()
                    || crate::plan::labels::parse_plan_owner(label).is_some()
                {
                    remove_labels.push(label.clone());
                }
            }
            let add_labels = vec![
                crate::plan::labels::plan_id(&plan_id),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
                owner_label,
            ];
            filter_remove_labels(&mut remove_labels, &add_labels);
            let update = spur_pm::IssueUpdate {
                add_labels,
                remove_labels,
                ..Default::default()
            };
            if let Err(error) = apply_issue_update(pm, &epic_id, update.clone()).await {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to persist execute_epic labels on epic: {error}"),
                );
            }
            rollback_updates.push((epic_id.clone(), invert_label_update(&update)));
        }

        for (issue_id, task_id, agent_name) in &task_scope {
            let issue = match pm.get_issue(issue_id).await {
                Ok(issue) => issue,
                Err(error) => {
                    self.plan_registry.lock().await.by_epic.remove(&epic_id);
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("failed to fetch execute_epic task '{issue_id}': {error}"),
                    );
                }
            };
            let update = replace_task_execution_labels(&issue, &plan_id, task_id, agent_name);
            if let Err(error) = apply_issue_update(pm, issue_id, update.clone()).await {
                let mut compensations = vec![(issue_id.clone(), invert_label_update(&update))];
                compensations.extend(rollback_updates.iter().rev().cloned());
                for (rollback_issue_id, rollback_update) in compensations {
                    if let Err(rollback_error) =
                        apply_issue_update(pm, &rollback_issue_id, rollback_update).await
                    {
                        tracing::warn!(
                            issue_id = %rollback_issue_id,
                            "failed to roll back execute_epic scope after task persist failure: {rollback_error}"
                        );
                    }
                }
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to persist execute_epic labels on task '{issue_id}': {error}"),
                );
            }
            rollback_updates.push((issue_id.clone(), invert_label_update(&update)));
        }

        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Some(adv) = pm.advanced() {
            let task_map = task_scope
                .iter()
                .map(|(issue_id, task_id, _)| (task_id.clone(), issue_id.clone()))
                .collect();
            let sg = EpicSubgraph {
                epic_id: epic_id.clone(),
                task_map,
            };
            emit_plan_submit_audit(
                adv,
                &plan_id,
                &sg,
                base_snapshot_branch.as_deref(),
                base_snapshot_oid.as_deref(),
                Some("execute_epic"),
                Some(self.brain_session_id().as_session_id()),
                None,
            )
            .await;

            if let Some(prior) = prior_owner_match.as_ref() {
                let audit = match prior {
                    crate::plan::ownership::PlanOwnerMatch::Unowned
                    | crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Some(
                        crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                            plan_id: plan_id.clone(),
                            owner: self.brain_session_id().to_string(),
                            token: uuid::Uuid::new_v4().to_string(),
                            reason: "execute_epic".to_string(),
                        },
                    ),
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Some(
                        crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipTransferred {
                            plan_id: plan_id.clone(),
                            from: owner.clone(),
                            to: self.brain_session_id().to_string(),
                            mode: "execute_epic".to_string(),
                            previous_token: String::new(),
                            new_token: uuid::Uuid::new_v4().to_string(),
                        },
                    ),
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        tracing::warn!(
                            target: "spur.audit.emit_skip",
                            epic_id = %epic_id,
                            plan_id = %plan_id,
                            owners = ?owners,
                            "execute_epic: prior epic labels ambiguous; skipping ownership audit emission"
                        );
                        None
                    }
                };
                if let Some(audit) = audit {
                    let kind_str = audit.kind_str();
                    let body = crate::plan::audit_sentinel::encode_comment(&audit);
                    if let Err(e) = adv.add_comment(&epic_id, &body).await {
                        tracing::warn!(
                            target: "spur.audit.emit_failure",
                            kind = kind_str,
                            epic_id = %epic_id,
                            plan_id = %plan_id,
                            "execute_epic ownership audit comment emission failed (owner label is persisted; audit missing): {e}"
                        );
                    }
                }
            }
        }

        // Insert into active_plans first (no registry lock held here).
        self.active_plans
            .lock()
            .await
            .insert(plan_id.clone(), Arc::clone(&state));

        // Replace the sentinel with the real plan_id now that dispatch is
        // committed. active_plans lock is already released above, so these
        // two locks are never held simultaneously.
        self.plan_registry
            .lock()
            .await
            .by_epic
            .insert(epic_id.clone(), plan_id.clone());

        if self.task_tracker.is_closed() {
            // Roll back: remove the active_plans entry we just inserted.
            {
                let mut plans = self.active_plans.lock().await;
                plans.remove(&plan_id);
            }
            // Roll back: remove the registry entry (real plan_id, not sentinel).
            {
                let mut reg = self.plan_registry.lock().await;
                reg.by_epic.remove(&epic_id);
            }
            return JsonRpcResponse::error(
                id,
                -32000,
                "orchestrator shutting down — execute_epic aborted",
            );
        }

        {
            let state = state.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        }
        self.fast_forward_reconciler();

        info!(
            plan_id = %plan_id,
            epic_id = %epic_id,
            tasks = task_count,
            "Epic plan submitted"
        );

        // 6. Build response: plan status + epic metadata.
        let status_val = {
            let st = state_for_status.lock().await;
            crate::plan::build_plan_status(&plan_id, &st)
        };

        let derived_info = json!({
            "task_count": task_count,
            "edge_count": derived.edge_count,
            "agents": derived.agent_counts,
            "warnings": derived.warnings,
        });

        let mut resp_val = status_val;
        if let serde_json::Value::Object(ref mut m) = resp_val {
            m.insert("epic_id".into(), serde_json::json!(epic_id));
            m.insert("derived".into(), derived_info);
        }

        let text = serde_json::to_string_pretty(&resp_val).unwrap_or_else(|_| resp_val.to_string());

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    async fn handle_get_plan_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::get_plan_status(self, &self.reconciler_outcomes, &ctx, args).await {
            Ok(status) => {
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("get_plan_status failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_get_reconciler_status(&self, id: Value) -> JsonRpcResponse {
        let status = self.reconciler_outcomes.lock().await.reconciler_status();
        let text = match serde_json::to_string_pretty(&status) {
            Ok(text) => text,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize reconciler status: {error}"),
                )
            }
        };

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    async fn handle_get_task_diff(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::get_task_diff(
            self.pm_service.as_deref(),
            self.feature_gate.as_ref(),
            self.repo_root.as_deref(),
            self,
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => {
                let text = match serde_json::to_string_pretty(&value) {
                    Ok(t) => t,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("get_task_diff response serialization failed: {e}"),
                        )
                    }
                };
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("get_task_diff failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_report_progress(&self, id: Value, args: Value) -> JsonRpcResponse {
        let sink = match self.event_sink.as_deref() {
            Some(sink) => sink,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "report_progress: event sink not configured",
                )
            }
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::report_progress(sink, &ctx, args).await {
            Ok(value) => {
                let text =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("report_progress failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
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

    async fn handle_preview_task_base(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::PreviewTaskBaseInput = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
        };

        match self.preview_task_base_impl(input).await {
            Ok(output) => match serde_json::to_string_pretty(&output) {
                Ok(text) => JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(error) => JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize preview_task_base response: {error}"),
                ),
            },
            Err(error) => {
                let message = error.to_string();
                if message.starts_with("unknown plan")
                    || message.starts_with("Unknown task_id")
                    || message.contains("Unknown plan_id")
                {
                    JsonRpcResponse::invalid_params(id, message)
                } else {
                    JsonRpcResponse::internal_error(id, message)
                }
            }
        }
    }

    async fn handle_review_task(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();
        let decision = args["decision"].as_str().ok_or("missing decision")?;
        let feedback = args["feedback"].as_str();

        let plan_arc = self.load_or_project_plan(&plan_id).await?;

        let sink: Option<&dyn crate::events::McpEventSink> = self.event_sink.as_deref();

        // INV-5: use handle_review_task so the plan lock is dropped before
        // pm.update_issue() is called.  The pm_service field stores a concrete
        // Arc<PmService>; coerce to Arc<dyn PmLike> so spawned completion
        // futures can emit audit sentinels after the lock is released.
        let pm_arc: Option<std::sync::Arc<dyn crate::plan::PmLike>> = self
            .pm_service
            .clone()
            .map(|s| s as std::sync::Arc<dyn crate::plan::PmLike>);

        let result = crate::plan::handle_review_task(
            Arc::clone(&plan_arc),
            &plan_id,
            &task_id,
            decision,
            feedback,
            pm_arc,
            sink,
            Some(&self.delegation_tx),
            Some(&self.task_tracker),
            Arc::clone(&self.feature_gate),
        )
        .await?;
        let mut result = result;

        if decision == "approve" {
            let clobber_report = self
                .run_clobber_detector_for_review(&plan_arc, &task_id)
                .await?;
            if !clobber_report.signals.is_empty() {
                if let serde_json::Value::Object(ref mut m) = result {
                    m.insert(
                        "signals".into(),
                        serde_json::to_value(&clobber_report.signals).unwrap_or(json!(null)),
                    );
                }
            }
            for warning in clobber_report.warnings {
                append_review_warning(&mut result, warning);
            }
        }

        if let Some(sink) = self.event_sink.as_deref() {
            let projected = self.load_or_project_plan(&plan_id).await?;
            let state = projected.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(Some(sink), &state);
        }

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
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
        self.active_plans
            .lock()
            .await
            .insert(plan_id, Arc::new(tokio::sync::Mutex::new(projected)));
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
        let persisted_cached = if let Some(existing) = cached.as_ref() {
            existing.lock().await.epic_id.is_some()
        } else {
            false
        };
        if let Some(existing) = cached.clone() {
            if !persisted_cached {
                return Ok(existing);
            }
        }

        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| format!("unknown plan '{plan_id}'"))?;
        let projected = crate::plan::projector::project_plan_from_beads(
            pm,
            plan_id,
            self.feature_gate.as_ref(),
        )
        .await
        .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
        self.install_projected_plan(projected, false).await;
        self.active_plans
            .lock()
            .await
            .get(plan_id)
            .cloned()
            .ok_or_else(|| format!("unknown plan '{plan_id}'"))
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
mod cancel_delegation_tests {
    use spur_acp::{CancelOutcome, CancellationControl};

    /// INV-6: CancellationControl.cancel returns Cancelled the first time
    /// and NotFound on a second call (token was removed on first cancel).
    #[tokio::test]
    async fn cancel_returns_cancelled_then_not_found() {
        let cc = CancellationControl::new();
        let token = cc.register("req-1".into()).await;

        assert!(!token.is_cancelled(), "token should not be cancelled yet");

        let outcome = cc.cancel("req-1").await;
        assert_eq!(outcome, CancelOutcome::Cancelled);
        assert!(
            token.is_cancelled(),
            "token must be cancelled after cancel()"
        );

        // Second cancel: token was removed, so NotFound.
        let outcome2 = cc.cancel("req-1").await;
        assert_eq!(outcome2, CancelOutcome::NotFound);
    }

    /// INV-6: cancel on an unknown id returns NotFound.
    #[tokio::test]
    async fn cancel_unknown_id_returns_not_found() {
        let cc = CancellationControl::new();
        let outcome = cc.cancel("no-such-id").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }

    /// INV-6: remove() cleans up without cancelling the token.
    #[tokio::test]
    async fn remove_cleans_up_without_cancelling() {
        let cc = CancellationControl::new();
        let token = cc.register("req-2".into()).await;
        cc.remove("req-2").await;
        assert!(!token.is_cancelled(), "remove must not cancel the token");
        // After remove, cancel returns NotFound.
        let outcome = cc.cancel("req-2").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }
}

#[cfg(test)]
mod retirement_state_tests {
    use std::future::pending;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use spur_acp::{BrainSessionId, SessionId};
    use tokio::sync::Notify;

    fn no_op_ctx() -> super::DetachedContinuationCtx {
        super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn test_server_mark_retiring_rejects_new_delegations() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        server.mark_retiring();

        let single = server
            .__test_call_delegate_to_worker("codex", "should reject")
            .await;
        assert_eq!(single["error"]["message"], "SessionRetiring");

        let parallel = server
            .__test_call_delegate_parallel(vec![("codex", "parallel should reject")])
            .await;
        assert_eq!(parallel["error"]["message"], "SessionRetiring");
    }

    #[tokio::test]
    async fn test_server_cancel_in_flight_signals_token() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        assert!(
            !server.cancel_token.is_cancelled(),
            "fresh servers must start with an active cancellation token"
        );

        server.cancel_in_flight_workers();

        assert!(
            server.cancel_token.is_cancelled(),
            "cancel_in_flight_workers must signal the shared cancellation token"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_idempotent() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());

        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            let started = Arc::clone(&started);
            async move {
                let _flag = DropFlag(dropped);
                started.notify_one();
                pending::<()>().await;
            }
        }));

        started.notified().await;
        server.force_abort();
        server.force_abort();
        tokio::time::timeout(Duration::from_millis(200), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("force_abort should eventually abort the stored root task");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must abort the stored root task"
        );
        assert!(
            server.root_handle.lock().unwrap().is_none(),
            "force_abort must take the root handle so repeated calls stay idempotent"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_after_shutdown_partial_progress() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let server = Arc::new(server);

        let release = Arc::new(Notify::new());
        server.task_tracker.spawn({
            let release = Arc::clone(&release);
            async move {
                release.notified().await;
            }
        });

        let dropped = Arc::new(AtomicBool::new(false));
        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                pending::<()>().await;
            }
        }));

        let shutdown = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                server.shutdown().await;
            }
        });

        tokio::task::yield_now().await;
        server.force_abort();
        release.notify_waiters();

        tokio::time::timeout(Duration::from_millis(200), shutdown)
            .await
            .expect("shutdown should complete once tracked work finishes")
            .expect("shutdown task must not panic");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must still abort the root task after shutdown has already started"
        );
    }
}

#[cfg(test)]
mod continuation_producer_tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use chrono::Utc;
    use spur_acp::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};
    use spur_acp::domain::continuation::ArtifactKind as ContinuationArtifactKind;
    use spur_acp::domain::events::{DiffSummary, SpurEventBody};
    use spur_acp::domain::{
        BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus,
    };
    use spur_acp::{DelegationId, SessionId};
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SpurEventBody>>,
    }

    impl crate::events::McpEventSink for RecordingSink {
        fn emit(&self, event: SpurEventBody) {
            self.events.lock().unwrap().push(event);
        }
    }

    async fn capture_continuation(
        delegation_id: DelegationId,
        result: DelegationResult,
        attempt: u32,
        brain_session: SessionId,
        event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    ) -> BrainContinuation {
        let tracker = TaskTracker::new();
        let active = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let completed = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_for_ctx = Arc::clone(&captured);
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let materializer = super::OutcomeMaterializer::new(store);

        let detached = Some(super::DetachedCompletionHandle {
            ctx: Arc::new(super::DetachedContinuationCtx {
                on_complete: Arc::new(move |cont, _worker_session| {
                    let captured = Arc::clone(&captured_for_ctx);
                    Box::pin(async move {
                        captured.lock().await.push(cont);
                    })
                }),
            }),
            source_kind: super::DetachedSourceKind::BlockTimeout,
            attempt_tracker: Arc::new(AtomicU32::new(attempt)),
            brain_session,
            event_sink,
            materializer,
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        super::McpCallbackServer::spawn_result_collector(
            &tracker,
            delegation_id,
            rx,
            CancellationToken::new(),
            active,
            completed,
            detached,
        );

        tx.send(result).expect("send continuation result");
        tracker.close();
        tracker.wait().await;

        let captured = captured.lock().await;
        assert_eq!(
            captured.len(),
            1,
            "collector should emit exactly one continuation"
        );
        captured[0].clone()
    }

    fn success_result(
        summary: Option<String>,
        diff_summary: Option<DiffSummary>,
        artifact: Option<WorkerArtifact>,
    ) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary,
            summary,
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-test".into()),
            artifact,
        }
    }

    #[tokio::test]
    async fn build_detached_continuation_populates_artifact_id_via_materializer() {
        use spur_blob_store::MemoryOutcomeStore;

        let store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = crate::outcome_materializer::OutcomeMaterializer::new(store);
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: None,
        };
        let delegation_id = DelegationId::from("deadbeef-1111-2222-3333-444455556666");
        let brain_session = SessionId("550e8400-e29b-41d4-a716-446655440000".into());

        let cont = super::build_detached_continuation(
            &delegation_id,
            &result,
            spur_acp::domain::ContinuationSource::BlockTimeout,
            1,
            brain_session,
            None,
            &mat,
        )
        .await;
        assert!(
            cont.payload.artifact_id.is_some(),
            "Phase 3 wires artifact_id"
        );
    }

    #[tokio::test]
    async fn test_producer_materializes_oversized_summary_with_fetch_hint() {
        let delegation_id: DelegationId = "del-oversized".into();
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let original_summary = "x".repeat(super::PRODUCER_MAX_FIELD_BYTES + 64);

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(Some(original_summary.clone()), None, None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        let clipped = continuation
            .payload
            .summary
            .as_ref()
            .expect("summary should still be present after clipping");
        assert!(
            clipped.len() <= super::PRODUCER_MAX_FIELD_BYTES,
            "clipped summary must stay within the producer byte budget"
        );
        assert!(
            clipped.ends_with('…'),
            "clipped summary should carry the ellipsis marker"
        );
        assert!(
            continuation.payload.artifact_id.is_some(),
            "full result should be fetchable from the outcome store"
        );
        assert!(
            continuation
                .payload
                .fetch_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("Summary truncated")),
            "fetch hint should tell the brain that the summary was clipped"
        );

        assert!(
            sink.events.lock().unwrap().is_empty(),
            "primary materializer path persists the full result instead of emitting a truncation event"
        );
    }

    #[tokio::test]
    async fn test_producer_diff_summary_handled() {
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let diff_summary = DiffSummary {
            files_changed: 2,
            insertions: 8,
            deletions: 3,
            files: vec!["src/main.rs".into(), "src/lib.rs".into()],
        };

        let continuation = capture_continuation(
            "del-diff-summary".into(),
            success_result(Some("ok".into()), Some(diff_summary.clone()), None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        assert_eq!(continuation.payload.diff_summary, Some(diff_summary));
        assert!(
            sink.events.lock().unwrap().is_empty(),
            "structured diff_summary should not emit truncation events when no string field is clipped"
        );
    }

    #[tokio::test]
    async fn test_continuation_construction_brain_session_attempt_created_at() {
        let delegation_id: DelegationId = "del-cont-1".into();
        let brain_session = SessionId("brain-session-7".into());
        let before_wall = Utc::now();
        let before_mono = Instant::now();

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(
                Some("done".into()),
                None,
                Some(WorkerArtifact {
                    object_ref: "refs/spur/artifacts/abc123".into(),
                    blob_sha: "0".repeat(40),
                    size_bytes: 1_234,
                    kind: WorkerArtifactKind::Diagnostic,
                }),
            ),
            7,
            brain_session.clone(),
            None,
        )
        .await;

        let after_mono = Instant::now();
        let after_wall = Utc::now();

        assert_eq!(continuation.delegation_id, delegation_id);
        assert_eq!(continuation.attempt, 7);
        assert_eq!(continuation.brain_session, brain_session);
        assert_eq!(continuation.source, ContinuationSource::BlockTimeout);
        assert!(continuation.created_at_wall >= before_wall);
        assert!(continuation.created_at_wall <= after_wall);
        assert!(continuation.created_at_mono >= before_mono);
        assert!(continuation.created_at_mono <= after_mono);

        let artifact_ref = continuation
            .payload
            .artifact_ref
            .as_ref()
            .expect("worker artifacts should map to continuation artifact refs");
        assert_eq!(
            artifact_ref.kind,
            ContinuationArtifactKind::Other("worker_artifact".into())
        );
        assert_eq!(artifact_ref.uri, "spur://artifact/del-cont-1");
        assert_eq!(artifact_ref.byte_size, 1_234);
        assert_eq!(
            artifact_ref.sha256.as_deref(),
            Some("0".repeat(40).as_str())
        );
        assert_eq!(
            artifact_ref.git_object_ref.as_deref(),
            Some("refs/spur/artifacts/abc123")
        );
        assert_eq!(
            artifact_ref.git_blob_sha.as_deref(),
            Some("0".repeat(40).as_str())
        );
    }
}

#[cfg(test)]
mod fetch_outcome_artifact_tests {
    //! End-to-end tests for the `fetch_outcome_artifact` MCP tool.
    //!
    //! Seeds the outcome store with serialized `DelegationResult` blobs,
    //! then calls the JSON-RPC tool dispatcher and asserts the section
    //! projection returned to the brain.

    use super::{DetachedContinuationCtx, McpCallbackServer};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use spur_acp::domain::{ContinuationSource, DelegationResult, DelegationStatus, OutcomeKey};
    use spur_acp::{BrainSessionId, DelegationId, SessionId};
    use spur_blob_store::{ContentType, OutcomeMetadata, OutcomeStore};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn init_git_repo(path: &Path) {
        let init = tokio::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .output()
            .await
            .expect("git init must run");
        assert!(init.status.success(), "git init failed: {init:?}");

        for kv in &[("user.email", "test@example.com"), ("user.name", "test")] {
            let out = tokio::process::Command::new("git")
                .args(["config", kv.0, kv.1])
                .current_dir(path)
                .output()
                .await
                .expect("git config must run");
            assert!(out.status.success(), "git config {} failed", kv.0);
        }
    }

    fn no_op_continuation_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_cont, _worker_session| Box::pin(async {})),
        }
    }

    async fn build_test_server(repo_root: &Path, session_id: &str) -> McpCallbackServer {
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        build_test_server_with_store(repo_root, brain_session, outcome_store).await
    }

    async fn build_test_server_with_store(
        repo_root: &Path,
        brain_session: BrainSessionId,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    ) -> McpCallbackServer {
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session),
            None,
            None,
            no_op_continuation_ctx(),
            outcome_store,
            super::community_feature_gate(),
        );
        server.set_repo_root(repo_root.to_path_buf());
        server
    }

    fn sha256_hex(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").expect("hex write infallible");
        }
        hex
    }

    fn outcome_metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Json,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    async fn put_outcome(
        store: &Arc<dyn OutcomeStore>,
        brain_session: &BrainSessionId,
        delegation_id: DelegationId,
        attempt: u32,
        result: &DelegationResult,
    ) {
        let bytes = serde_json::to_vec(result).expect("serialize result");
        let metadata = outcome_metadata(&bytes);
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id,
            attempt,
        };
        store
            .put(&key, &bytes, &metadata)
            .await
            .expect("put outcome");
    }

    fn success_result(summary: &str, diff: &str, cost: f64) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            summary: Some(summary.into()),
            diff: Some(diff.into()),
            diff_summary: None,
            estimated_cost_usd: cost,
            worker_branch: None,
            artifact: None,
        }
    }

    fn dispatch_args(name: &str, args: Value) -> Value {
        json!({ "name": name, "arguments": args })
    }

    fn response_text(response: &super::JsonRpcResponse) -> &str {
        response.result.as_ref().expect("expected success response")["content"][0]["text"]
            .as_str()
            .expect("text content")
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_persisted_blob_text() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let result = success_result("ok", "line one\nline two\n", 0.0);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_id.as_str() }),
                ),
            )
            .await;

        let text = response_text(&response);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result json");
        assert_eq!(parsed.summary.as_deref(), Some("ok"));
        assert_eq!(parsed.diff.as_deref(), Some("line one\nline two\n"));
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_status_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-status-only".into();
        let result = success_result("summary must stay out", "diff must stay out", 1.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "status_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["estimated_cost_micros"], 1_250_000);
        assert!(projected.get("summary").is_none());
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_summary_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-summary".into();
        let result = success_result("summary included", "diff must stay out", 0.5);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["summary"], "summary included");
        assert_eq!(projected["estimated_cost_micros"], 500_000);
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_diff_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-diff-only".into();
        let result = success_result("summary must stay out", "diff included", 0.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "diff_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["diff"], "diff included");
        assert!(projected.get("diff_summary").is_some());
        assert!(projected.get("summary").is_none());
        assert!(projected.get("attempt").is_none());
        assert!(projected.get("estimated_cost_micros").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_pins_specific_attempt() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-attempts".into();
        server
            .materializer
            .materialize(
                success_result("attempt one", "diff one", 0.0),
                delegation_id.clone(),
                1,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        server
            .materializer
            .materialize(
                success_result("attempt two", "diff two", 0.0),
                delegation_id.clone(),
                2,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let latest_response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let latest: Value = serde_json::from_str(response_text(&latest_response)).expect("json");
        assert_eq!(latest["attempt"], 2);
        assert_eq!(latest["summary"], "attempt two");

        let pinned_response = server
            .handle_tool_call(
                Value::Number(2.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let pinned: Value = serde_json::from_str(response_text(&pinned_response)).expect("json");
        assert_eq!(pinned["attempt"], 1);
        assert_eq!(pinned["summary"], "attempt one");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_invalid_attempt_arg() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        for invalid in [json!(-1), json!("two"), json!(0), json!(false)] {
            let response = server
                .handle_tool_call(
                    Value::Number(1.into()),
                    dispatch_args(
                        "fetch_outcome_artifact",
                        json!({
                            "delegation_id": "deadbeef-1111-2222-3333-444455556666",
                            "attempt": invalid,
                        }),
                    ),
                )
                .await;
            let error = response
                .error
                .as_ref()
                .unwrap_or_else(|| panic!("expected InvalidParams for attempt={invalid:?}"));
            assert_eq!(error.code, -32602);
            assert!(
                error.message.contains("Invalid 'attempt'"),
                "expected attempt rejection, got: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_internal_error_on_corrupted_blob() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        // Seed the store with bytes that ARE valid UTF-8 but NOT a valid
        // DelegationResult — exercises ProjectionError::InvalidResult on
        // a non-Full projection.
        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id: delegation_id.clone(),
            attempt: 1,
        };
        let bytes = b"not a delegation result";
        let metadata = outcome_metadata(bytes);
        store.put(&key, bytes, &metadata).await.expect("put");

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let error = response
            .error
            .as_ref()
            .expect("expected InternalError on corrupted blob");
        assert_eq!(error.code, -32603, "InternalError JSON-RPC code");
        assert!(
            error.message.to_lowercase().contains("projection")
                || error.message.contains("DelegationResult"),
            "expected projection-error context: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_clean_error_for_unknown_delegation() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": "nonexistent-delegation-id" }),
                ),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        // Phase 2 Task 10: a missing artifact is reported as Unauthorized
        // rather than NotFound so that a caller cannot probe whether a given
        // (delegation_id, attempt) exists in another brain session.
        assert_eq!(error.code, -32001);
        assert!(
            error.message.contains("not accessible"),
            "error message must mention not-accessible: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_unknown_section_cleanly() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": "any-id",
                        "section": "not_a_section"
                    }),
                ),
            )
            .await;

        let error = response
            .error
            .as_ref()
            .expect("expected InvalidParams error");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
        assert!(
            error
                .message
                .contains("Must be one of: status_only, summary, diff_only, full"),
            "unknown sections must be rejected cleanly: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_empty_delegation_id() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args("fetch_outcome_artifact", json!({ "delegation_id": "" })),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_completed_delegations_are_per_session() {
        // Two MCP servers share the same store, but each binds fetches to
        // its own brain_session_id. Server B asks for the same delegation_id
        // under its session and must not see Server A's outcome.
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_a_id = "550e8400-e29b-41d4-a716-446655440000";
        let session_b_id = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let brain_session_a = BrainSessionId::new(SessionId(session_a_id.into()));
        let brain_session_b = BrainSessionId::new(SessionId(session_b_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());

        let server_a =
            build_test_server_with_store(td.path(), brain_session_a.clone(), store.clone()).await;
        let server_b =
            build_test_server_with_store(td.path(), brain_session_b, store.clone()).await;

        let delegation_a: DelegationId = "delegation-belonging-to-a".into();
        let result_a = success_result("secret stdout for session A", "secret diff", 0.0);
        put_outcome(&store, &brain_session_a, delegation_a.clone(), 1, &result_a).await;

        // Server A can fetch its own delegation.
        let resp_a = server_a
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let text = response_text(&resp_a);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result");
        assert_eq!(
            parsed.summary.as_deref(),
            Some("secret stdout for session A")
        );

        // Server B fetches under its own brain_session_id and is denied as
        // Unauthorized — the store-miss is deliberately indistinguishable
        // from a "different session" miss to prevent cross-session probing.
        let resp_b = server_b
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let err = resp_b.error.as_ref().expect("server B must error");
        assert_eq!(err.code, -32001);
        assert!(
            err.message.contains("not accessible"),
            "Server B must not expose Server A's delegations: {err:?}"
        );
    }
}

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
            Some("spur/brain-snapshot-test"),
            Some(base_snapshot_oid.as_str()),
            Some("submit_plan"),
            None,
            None,
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
            Some("spur/brain-snapshot-test"),
            Some(base_snapshot_oid.as_str()),
            Some("submit_plan"),
            None,
            None,
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
            Some("spur/brain-snapshot-test"),
            Some(base_snapshot_oid.as_str()),
            Some("submit_plan"),
            None,
            None,
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

#[cfg(test)]
mod reconciler_fast_forward_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Notify;

    #[tokio::test]
    async fn notify_fast_forward_wakes_waiter() {
        let notify = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        super::notify_fast_forward(&Some(Arc::clone(&notify)));

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("waiter must wake")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_configured_notify() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let notify = Arc::new(Notify::new());
        server.set_reconciler_enabled(true, Some(Arc::clone(&notify)));

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the configured reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_default_notify_when_enabled_without_config() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.set_reconciler_enabled(true, None);
        let notify = server
            .reconciler_fast_forward
            .as_ref()
            .cloned()
            .expect("default fast-forward notify should be allocated");

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the default reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn load_or_project_plan_returns_cached_entry_when_present() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let plan = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server
            .active_plans
            .lock()
            .await
            .insert("plan-1".into(), Arc::clone(&plan));

        let loaded = server
            .load_or_project_plan("plan-1")
            .await
            .expect("load cached plan");
        assert!(Arc::ptr_eq(&loaded, &plan));
    }

    #[test]
    fn discover_plan_ids_collects_unique_prefix_values() {
        let issues = vec![
            spur_pm::IssueSummary {
                id: "bd-1".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic A".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-1"),
                    crate::plan::labels::PLAN_COMPLETE.to_string(),
                ],
                url: "beads://bd-1".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
            },
            spur_pm::IssueSummary {
                id: "bd-2".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic B".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-2"),
                    crate::plan::labels::plan_id("plan-1"),
                ],
                url: "beads://bd-2".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
            },
        ];

        let plan_ids = super::discover_plan_ids(&issues);
        assert_eq!(plan_ids, vec!["plan-1".to_string()]);
    }

    #[test]
    fn mutation_orphan_ids_require_terminal_companion_breadcrumb() {
        use crate::plan::audit_sentinel::AuditSentinelKind;

        let audits = vec![
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-1".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-2".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-2".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationCommit {
                mutation_id: "mut-2".into(),
                children_created: vec!["bd-2".into()],
            },
        ];

        assert_eq!(
            super::mutation_orphan_ids(&audits),
            vec!["mut-1".to_string()]
        );
    }

    #[test]
    fn execution_label_replacement_removes_old_plan_and_agent_labels() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("old-plan"),
                crate::plan::labels::agent("old-agent"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "new-plan", "codex");
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::plan_id("new-plan")));
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::agent("codex")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::plan_id("old-plan")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::agent("old-agent")));
    }

    /// Regression for bd-19od: when an issue already carries the correct
    /// `spur:agent:<name>` and/or `spur:plan-id:<id>` label, the same string
    /// must NOT appear in both `add_labels` and `remove_labels`. The beads
    /// CLI processes adds before removes, so the duplicate would strip the
    /// label we just (idempotently) added.
    #[test]
    fn execution_label_replacement_does_not_strip_already_correct_agent_label() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::agent("claude-code"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "plan-1", "claude-code");
        let agent_label = crate::plan::labels::agent("claude-code");
        let plan_label = crate::plan::labels::plan_id("plan-1");
        assert!(
            update.add_labels.contains(&agent_label),
            "add_labels must include the target agent label: {:?}",
            update.add_labels
        );
        assert!(
            !update.remove_labels.contains(&agent_label),
            "remove_labels must NOT contain the agent label that we are also adding: {:?}",
            update.remove_labels
        );
        assert!(
            !update.remove_labels.contains(&plan_label),
            "remove_labels must NOT contain the plan-id label that we are also adding: {:?}",
            update.remove_labels
        );

        let task_update =
            super::replace_task_execution_labels(&issue, "plan-1", "t1", "claude-code");
        assert!(
            !task_update.remove_labels.contains(&agent_label),
            "replace_task_execution_labels must also filter the agent label: {:?}",
            task_update.remove_labels
        );
        assert!(
            !task_update.remove_labels.contains(&plan_label),
            "replace_task_execution_labels must also filter the plan-id label: {:?}",
            task_update.remove_labels
        );
    }

    #[test]
    fn persisted_plan_epic_blocks_execute_epic_relabeling() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Persisted plan epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), Some("plan-1"));
    }

    #[test]
    fn ordinary_epic_can_still_be_executed() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Product epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), None);
    }

    #[tokio::test]
    async fn install_projected_plan_replaces_stale_cache_entry() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        let stale = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server
            .active_plans
            .lock()
            .await
            .insert("plan-1".into(), Arc::clone(&stale));

        let fresh = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: Some("refs/heads/main".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh, false).await;
        let loaded = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached plan");
        assert_eq!(loaded.lock().await.tasks.len(), 1);
    }

    #[tokio::test]
    async fn reclaim_persisted_plans_hydrates_empty_cache() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner(
                    &session_id.as_session_id().0,
                )],
                ..Default::default()
            },
        )
        .await
        .expect("stamp owner label");

        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        assert!(server.active_plans.lock().await.is_empty());

        server
            .reclaim_persisted_plans_on_startup(pm)
            .await
            .expect("reclaim persisted plans");
        assert!(!server.active_plans.lock().await.is_empty());
    }

    #[tokio::test]
    async fn reclaim_replaces_existing_cache_entry_instead_of_merging() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
                plan_id: "plan-1".into(),
                tasks: Vec::new(),
                brain_session_id: session_id.clone(),
                base_snapshot_branch: None,
                base_snapshot_oid: None,
                merge_state: crate::plan::PlanMergeState::NotStarted,
                epic_id: None,
            })),
        );

        let fresh_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };
        let replacement_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t1".into(),
                        agent: "codex".into(),
                        task: "Task".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-1".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Ready,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t2".into(),
                        agent: "codex".into(),
                        task: "Task 2".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-2".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Pending,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
            ],
            brain_session_id: session_id,
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh_plan, false).await;
        server.install_projected_plan(replacement_plan, false).await;
        let cached = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached");
        assert_eq!(cached.lock().await.tasks.len(), 2);
    }

    #[tokio::test]
    async fn detector_skips_reclaim_when_all_epics_have_rev1_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit so the epic carries rev1 bootstrap metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(
            adv,
            "plan-1",
            &sg,
            Some("main"),
            Some("abc123"),
            Some("test"),
            None,
            None,
        )
        .await;

        // The detector must report that no legacy reclaim is needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            !needs_reclaim,
            "detector must skip reclaim when all epics have rev1 metadata"
        );
    }

    #[tokio::test]
    async fn detector_reclaims_when_plan_submit_lacks_bootstrap_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit WITHOUT base snapshot metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(adv, "plan-1", &sg, None, None, None, None, None).await;

        // The detector must report that legacy reclaim is still needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            needs_reclaim,
            "detector must reclaim when PlanSubmit lacks rev1 bootstrap metadata"
        );
    }

    #[test]
    fn legacy_reclaim_needed_when_rev1_bootstrap_metadata_is_missing() {
        assert!(super::legacy_reclaim_needed(false));
    }

    #[test]
    fn legacy_reclaim_skipped_when_rev1_bootstrap_metadata_exists() {
        assert!(!super::legacy_reclaim_needed(true));
    }
}
