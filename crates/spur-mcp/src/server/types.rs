use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BeadsVersion {
    /// The persisted plan version could not be derived or is intentionally
    /// unknown for an ephemeral plan. This is distinct from `AuditSeq(0)`,
    /// which is a real persisted epic with zero audit sentinels.
    Unknown,
    /// Legacy monotonic audit-sentinel sequence number.
    /// Kept for backwards compatibility with existing caches/tests.
    AuditSeq(u64),
    /// Content-addressed token over all `[[spur-audit v1]]` comment IDs
    /// visible to plan projection (`spur:plan-id:<id>` scope).
    ContentHash([u8; 32]),
}

#[derive(Debug, Clone)]
pub(crate) struct CachedPlan {
    pub(crate) state: Arc<tokio::sync::Mutex<crate::plan::PlanState>>,
    pub(crate) beads_version: BeadsVersion,
    pub(crate) cached_at: Instant,
}

impl CachedPlan {
    pub(crate) fn new(
        state: Arc<tokio::sync::Mutex<crate::plan::PlanState>>,
        beads_version: BeadsVersion,
    ) -> Self {
        Self {
            state,
            beads_version,
            cached_at: Instant::now(),
        }
    }
}

pub(crate) fn unknown_beads_version() -> BeadsVersion {
    BeadsVersion::Unknown
}

pub(crate) const VERSIONED_PLAN_CACHE_MAX_ATTEMPTS: usize = 3;
pub(crate) const VERSIONED_PLAN_CACHE_BACKOFFS: [std::time::Duration;
    VERSIONED_PLAN_CACHE_MAX_ATTEMPTS] = [
    std::time::Duration::from_millis(100),
    std::time::Duration::from_millis(500),
    std::time::Duration::from_millis(2_000),
];
pub(crate) const UNVERSIONED_PLAN_CACHE_REFRESH_AFTER: std::time::Duration =
    std::time::Duration::from_millis(0);
pub(crate) const UNVERSIONED_PLAN_CACHE_INLINE_REFRESH_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(250);

/// How long completed delegation results are retained before lazy eviction.
///
/// Phase 4: the `completed_delegations` map is preserved as a TTL-bounded
/// debug buffer (per the async-first design spec). After Part A removed
/// `delegate_async` / `wait_delegation`, no handler writes the map under
/// normal operation — `BlockTimeout` collectors skip it (INV-ASYNC-2).
/// The 60 s TTL is generous for any residual debug-injection use; the
/// map is allowed to stay permanently empty in production.
pub(crate) const COMPLETED_TTL: std::time::Duration = std::time::Duration::from_secs(60);
pub(crate) const DEFAULT_PLAN_PENDING_GRACE: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);
pub(crate) const BRAIN_SESSION_BIND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
pub(crate) const PERSIST_AS_EPIC_FALSE_REMOVED_MESSAGE: &str = "persist_as_epic=false is removed; remove the field — true is the only supported value. See docs/superpowers/specs/2026-05-10-submit-plan-substrate-migration-design.md.";
/// Prefix used to mark a comment as a startup-sweep quarantine audit.
///
/// **DO NOT RENAME WITHOUT MIGRATION.** This string is durable state: the
/// startup sweep retry path (`pending_sweep_allows_child_status`) treats the
/// presence of a comment with this prefix as proof that a child was
/// quarantined by a previous sweep run. Changing this constant will break
/// resumption of any sweep that was interrupted under the old value.
pub(crate) const PLAN_PENDING_SWEEP_COMMENT_PREFIX: &str =
    "SPUR startup sweep quarantined stale pending plan";
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
pub(crate) const MCP_SESSION_KEEPALIVE_DEFAULT: std::time::Duration =
    std::time::Duration::from_secs(4 * 60 * 60);

pub(crate) fn mcp_session_keepalive() -> Option<std::time::Duration> {
    match std::env::var("SPUR_MCP_SESSION_KEEPALIVE_SECS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(std::time::Duration::from_secs(secs)),
            Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
        },
        Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
    }
}

pub(crate) struct ReconcilerTaskHandle {
    pub(crate) cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) handle: AbortOnDropHandle<()>,
}

impl ReconcilerTaskHandle {
    pub(crate) fn abort(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }

    pub(crate) async fn shutdown(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }
}

pub(crate) struct StartupRecoveryTaskHandle {
    pub(crate) cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) handle: AbortOnDropHandle<()>,
}

impl StartupRecoveryTaskHandle {
    pub(crate) fn abort(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }

    pub(crate) async fn shutdown(mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }

    pub(crate) async fn wait(self) {
        let _ = self.handle.await;
    }
}

#[derive(Default)]
pub(crate) struct StartupRecoveryState {
    pub(crate) pending: bool,
    pub(crate) handle: Option<StartupRecoveryTaskHandle>,
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

    pub(crate) async fn pause(self: Arc<Self>) {
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
pub(crate) static STARTUP_RECOVERY_PROBE: Mutex<Option<Arc<StartupRecoveryProbe>>> =
    Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn pause_startup_recovery_if_probed() {
    let probe = STARTUP_RECOVERY_PROBE.lock().unwrap().clone();
    if let Some(probe) = probe {
        probe.pause().await;
    }
}

#[cfg(test)]
pub(crate) const PRODUCER_MAX_FIELD_BYTES: usize = 8192;
pub(crate) const MCP_NOT_LICENSED_ERROR_CODE: i32 = -32041;

// ─── JSON-RPC types ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub(crate) jsonrpc: String,
    pub(crate) id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
}

impl JsonRpcError {
    pub(crate) fn into_mcp_error(self) -> McpError {
        McpError::new(
            rmcp::model::ErrorCode(self.code as i32),
            self.message,
            self.data,
        )
    }
}

impl JsonRpcResponse {
    pub(crate) fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
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

    pub(crate) fn error_with_data(
        id: Value,
        code: i64,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }

    pub(crate) fn invalid_params(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32602, msg)
    }

    pub(crate) fn internal_error(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32603, msg)
    }

    pub(crate) fn mcp_error(id: Value, error: McpError) -> Self {
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
pub(crate) fn dispatch_error_response(err: DelegationDispatchError, id: Value) -> JsonRpcResponse {
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

pub(crate) fn new_attempt_tracker() -> Arc<AtomicU32> {
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
pub(crate) fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
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
            prior_branch_for_reuse: None,
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

#[derive(Debug, Default)]
pub(crate) struct ClobberReviewReport {
    pub(crate) signals: Vec<crate::plan::signals::WorkerSignal>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn append_review_warning(resp: &mut serde_json::Value, warning: String) {
    if let serde_json::Value::Object(map) = resp {
        match map.get_mut("warnings") {
            Some(serde_json::Value::Array(warnings)) => warnings.push(json!(warning)),
            _ => {
                map.insert("warnings".into(), json!([warning]));
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn unlicensed_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = community_feature_gate();
    let mut snapshot = (**gate.snapshot()).clone();
    snapshot
        .features
        .remove(&spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED);
    gate.set_snapshot_for_test(snapshot);
    gate
}
