//! Per-`BrainSession` HTTP/JSON-RPC server exposing the curated worker MCP
//! tool subset to delegated workers.
//!
//! The transport layer is RMCP Streamable HTTP (`axum` + `StreamableHttpService`)
//! and tools are exposed as first-class RMCP tool methods that delegate to the
//! existing freestanding handlers in [`crate::handlers`]. Audit emission and
//! per-delegation lifecycle guards remain in this module.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Configuration for the background audit flusher and idle thresholds.
#[derive(Debug, Clone, Copy)]
pub struct WorkerMcpServerConfig {
    /// How long a read-audit buffer must be untouched before the background
    /// flusher considers it idle and emits a `ReadAggregate` sentinel.
    pub idle_threshold: Duration,
    /// How often the flusher scans the per-delegation buffer map.
    pub scan_interval: Duration,
}

impl Default for WorkerMcpServerConfig {
    fn default() -> Self {
        Self {
            idle_threshold: Duration::from_secs(30),
            scan_interval: Duration::from_secs(10),
        }
    }
}

use parking_lot::Mutex;
use rand::RngCore;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, Implementation, JsonObject, ServerCapabilities, ServerInfo},
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::{local::LocalSessionManager, SessionId, SessionManager},
        StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, Response, StatusCode},
    middleware::{self, Next},
    Router,
};

use crate::events::McpEventSink;
use crate::handlers::{McpHandlerError, PlanResolver, WorkerCallContext};
use crate::outcome_materializer::OutcomeMaterializer;
use crate::token::validate_token;

/// Per-delegation context cached by the dispatcher so gating checks never
/// hit the PM on the hot path.
#[derive(Debug, Clone, Copy, Default)]
pub struct DelegationContext {
    pub enable_worker_progress: bool,
}

/// One read-tool call recorded for later aggregation into a single
/// `worker-mcp` audit comment by the background flusher (T21).
///
/// Read tools (`get_issue`, `list_issues`, `get_task_diff`,
/// `get_plan_status`, `fetch_outcome_artifact`) are coalesced per delegation
/// rather than producing one beads comment per call so the audit trail stays
/// readable.
#[derive(Debug, Clone)]
pub struct ReadAuditEntry {
    pub tool_name: String,
    pub target_issue_id: Option<String>,
    /// Unix seconds, captured at append time.
    pub ts: u64,
}

/// Drained payload sent across the flush channel to the background audit
/// task. Carries the full delegation_id so the receiver can route the
/// aggregated comment to the correct beads issue.
#[derive(Debug)]
pub struct FlushMessage {
    pub delegation_id: String,
    pub entries: Vec<ReadAuditEntry>,
}

/// Per-delegation aggregation buffer for read-tool audit entries.
///
/// Held inside the dispatcher's `read_audit_buffers` map keyed by
/// `delegation_id`. Each read-tool call appends a [`ReadAuditEntry`]; the
/// background flusher (T21) drains the buffer either on idle timeout or on
/// explicit delegation completion. As a safety net, the synchronous [`Drop`]
/// impl performs a non-blocking `send` on the flush channel so entries are
/// not lost even if the buffer is removed from the map without an explicit
/// flush call.
pub struct ReadAuditBuffer {
    delegation_id: String,
    entries: Mutex<Vec<ReadAuditEntry>>,
    flush_tx: mpsc::UnboundedSender<FlushMessage>,
}

impl ReadAuditBuffer {
    pub fn new(delegation_id: String, flush_tx: mpsc::UnboundedSender<FlushMessage>) -> Self {
        Self {
            delegation_id,
            entries: Mutex::new(Vec::new()),
            flush_tx,
        }
    }

    /// Append an entry. O(1) amortized; only blocks on the per-buffer lock,
    /// never on the flush channel.
    pub fn append(&self, entry: ReadAuditEntry) {
        self.entries.lock().push(entry);
    }

    pub fn entry_count(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn delegation_id(&self) -> &str {
        &self.delegation_id
    }

    /// Drain the buffer and return the accumulated entries. Used by
    /// [`WorkerMcpServer::flush_delegation`] to forward entries on the
    /// flush channel in-band rather than relying on the buffer's `Drop`.
    pub fn take_entries(&self) -> Vec<ReadAuditEntry> {
        std::mem::take(&mut *self.entries.lock())
    }

    /// Test-only escape hatch so unit tests can populate the buffer without
    /// going through the dispatcher.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_for_test(&self, entry: ReadAuditEntry) {
        self.append(entry);
    }
}

impl Drop for ReadAuditBuffer {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut *self.entries.lock());
        if entries.is_empty() {
            return;
        }
        // `mpsc::UnboundedSender::send` never blocks. If the receiver has
        // been dropped we silently swallow the error: there's no actor left
        // to deliver the audit to, and panicking inside `Drop` would abort
        // the process. The background flusher (T21) keeps the receiver
        // alive for the server's lifetime.
        let _ = self.flush_tx.send(FlushMessage {
            delegation_id: std::mem::take(&mut self.delegation_id),
            entries,
        });
    }
}

/// Shared dependencies the dispatcher hands to handlers. Bundled into a
/// struct so [`WorkerMcpServer::start`] stays one positional arg per
/// orthogonal concept and so future tasks (T18 dual-gating, T19 audit) can
/// extend the set without churning every call site.
#[derive(Clone)]
pub struct WorkerMcpDeps {
    pub pm_service: Arc<spur_pm::PmService>,
    pub feature_gate: Arc<spur_license::FeatureGate>,
    pub funnel: Arc<dyn McpEventSink>,
    pub plan_resolver: Arc<dyn PlanResolver>,
    pub reconciler_outcomes: Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>>,
    pub outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    /// Required by `get_task_diff` when reconstructing diffs from persisted
    /// worker branches; `None` disables that recovery branch.
    pub repo_root: Option<PathBuf>,
}

/// Per-`BrainSession` worker MCP server. Owns a TCP listener on
/// `127.0.0.1:<random-port>` and a cancellable accept loop.
pub struct WorkerMcpServer {
    addr: SocketAddr,
    /// 32-byte HMAC key generated at start. Held in-process only — never
    /// logged or persisted. Used by token validation middleware (T16+).
    hmac_key: [u8; 32],
    /// `BrainSession` id stamped onto every `WorkerCallContext` issued by this
    /// server (T16+).
    brain_session_id: String,
    /// Materializer is derived from `outcome_store` once at start so the
    /// dispatcher hot path doesn't pay for the construction per request.
    deps: Arc<DispatcherDeps>,
    shutdown: CancellationToken,
    session_manager: Arc<LocalSessionManager>,
    session_contexts:
        Arc<parking_lot::Mutex<std::collections::HashMap<String, AuthenticatedWorkerContext>>>,
    accept_loop_handle: Mutex<Option<JoinHandle<()>>>,
    flusher_handle: Mutex<Option<JoinHandle<()>>>,
    /// Receiver half of the read-audit flush channel. Created in `start()`
    /// and owned here until T21 spawns the background flusher task that
    /// `take()`s it. Holding the receiver here prevents the channel from
    /// closing prematurely during the T20→T21 transitional window.
    flush_rx: Mutex<Option<mpsc::UnboundedReceiver<FlushMessage>>>,
    /// In-flight dispatcher count. Incremented on `dispatch` entry by
    /// [`ActiveCallGuard`] and decremented on its `Drop`. [`shutdown`]
    /// polls this counter (via [`active_count`](Self::active_count)) so
    /// in-flight requests are drained before the flusher task is joined.
    /// `Arc` so the guard owned by each dispatch task can decrement
    /// independently of the server's lifetime.
    active_delegations: Arc<AtomicU32>,
}

/// RAII guard that increments [`WorkerMcpServer::active_delegations`] on
/// construction and decrements on drop. Created at the top of [`dispatch`]
/// so the count is panic-safe — even if a handler panics between increment
/// and decrement, unwinding through `Drop` keeps the counter consistent.
/// This mirrors the [`ReadAuditBuffer`] Drop pattern used by T20.
struct ActiveCallGuard {
    counter: Arc<AtomicU32>,
}

impl ActiveCallGuard {
    fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self { counter }
    }
}

impl Drop for ActiveCallGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One worker-MCP call recorded on the per-delegation summary guard.
struct CallRecord {
    tool_name: String,
    latency_ms: u64,
    is_error: bool,
}

/// Per-delegation tracker that emits a `WorkerMcpDelegationSummary` event on
/// Drop. Held in [`DispatcherDeps::delegation_guards`] and removed by
/// [`WorkerMcpServer::complete_delegation`] (or on server shutdown) so the
/// summary fires exactly once per delegation.
pub struct DelegationDispatchGuard {
    delegation_id: String,
    brain_session_id: String,
    funnel: Arc<dyn McpEventSink>,
    calls: Mutex<Vec<CallRecord>>,
    /// Delegation-level error count, separate from per-call `is_error`.
    /// Bumped by [`mark_error`] when the brain reports a terminal error
    /// outcome or the flush channel closes; added to the per-call error
    /// count in the emitted `WorkerMcpDelegationSummary`. This preserves
    /// the delegation-level outcome bit even when no per-call errors
    /// occurred (e.g. clean dispatch, then flush channel closure).
    extra_errors: AtomicU64,
    /// Set to `true` by [`WorkerMcpServer::complete_delegation`] so the
    /// summary only fires on explicit delegation close, not on server
    /// shutdown or map eviction.
    completed: AtomicBool,
}

impl DelegationDispatchGuard {
    pub fn new(
        delegation_id: String,
        brain_session_id: String,
        funnel: Arc<dyn McpEventSink>,
    ) -> Self {
        Self {
            delegation_id,
            brain_session_id,
            funnel,
            calls: Mutex::new(Vec::new()),
            extra_errors: AtomicU64::new(0),
            completed: AtomicBool::new(false),
        }
    }

    /// Record one completed worker-MCP call with its tool name, observed
    /// latency, and whether it returned an error to the worker.
    pub fn record_call(&self, tool_name: &str, latency_ms: u64, is_error: bool) {
        self.calls.lock().push(CallRecord {
            tool_name: tool_name.to_string(),
            latency_ms,
            is_error,
        });
    }

    /// Bump a delegation-level error counter. Used by
    /// [`WorkerMcpServer::flush_delegation`] when the brain reports a
    /// terminal error outcome or the audit-flush channel closes — so the
    /// emitted `WorkerMcpDelegationSummary.errors` reflects the
    /// delegation-level failure even when no per-call errors occurred.
    pub fn mark_error(&self) {
        self.extra_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Compute p99 latency from a slice of successful-call latencies in
/// milliseconds. Per spec: if fewer than 2 samples, return the max observed
/// (or 0 when empty). Otherwise use nearest-rank: `idx = ceil(0.99 * n) - 1`.
fn compute_p99_latency_ms(latencies: &[u64]) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u64> = latencies.to_vec();
    sorted.sort_unstable();
    if sorted.len() < 2 {
        return *sorted.last().expect("len >= 1");
    }
    let n = sorted.len();
    let rank = ((n as f64) * 0.99).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

impl Drop for DelegationDispatchGuard {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Relaxed) {
            return;
        }
        let calls = self.calls.lock();
        let calls_total = calls.len() as u64;
        let mut calls_by_tool: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        let mut success_latencies: Vec<u64> = Vec::with_capacity(calls.len());
        let mut errors: u64 = 0;
        for call in calls.iter() {
            *calls_by_tool.entry(call.tool_name.clone()).or_insert(0) += 1;
            if call.is_error {
                errors += 1;
            } else {
                success_latencies.push(call.latency_ms);
            }
        }
        let p99_latency_ms = compute_p99_latency_ms(&success_latencies);
        // Add delegation-level errors (set via `mark_error` from
        // `flush_delegation` on terminal-error outcome or flush channel
        // closure) so the summary reflects failures even when no
        // per-call errors occurred.
        let errors = errors + self.extra_errors.load(Ordering::Relaxed);
        let body = spur_acp::SpurEventBody::WorkerMcpDelegationSummary {
            delegation_id: self.delegation_id.clone(),
            brain_session_id: self.brain_session_id.clone(),
            calls_total,
            calls_by_tool,
            p99_latency_ms,
            errors,
        };
        // `try_emit` so a full broadcast bus doesn't back-pressure the Drop.
        let _ = self.funnel.try_emit(body);
    }
}

/// Internal bundle of the materialized handler dependencies. Mirrors
/// [`WorkerMcpDeps`] plus the derived [`OutcomeMaterializer`].
struct DispatcherDeps {
    pm_service: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    funnel: Arc<dyn McpEventSink>,
    plan_resolver: Arc<dyn PlanResolver>,
    reconciler_outcomes: Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>>,
    outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    materializer: OutcomeMaterializer,
    repo_root: Option<PathBuf>,
    /// Cached per-delegation context — populated by the orchestrator at
    /// dispatch time so gating checks are O(1) and PM-free.
    /// TODO(T22/T24): add unregister_delegation() called by the orchestrator on
    /// terminal delegation status (Success/Failed/Cancelled) so long-running
    /// brain sessions don't leak DelegationContext entries indefinitely.
    delegations: Arc<parking_lot::Mutex<std::collections::HashMap<String, DelegationContext>>>,
    /// Per-delegation read-tool aggregation buffers. Each entry is `Arc`'d
    /// so concurrent in-flight read calls and the (future T21) background
    /// flusher can hold cheap references without contending for the outer
    /// map lock. Keyed by `delegation_id`.
    /// TODO(T21): the background flusher periodically removes idle entries
    /// (last-touched > N minutes) so this map doesn't grow unbounded across
    /// long-lived brain sessions.
    read_audit_buffers:
        Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<ReadAuditBuffer>>>>,
    /// Sender half of the read-audit flush channel. Cloned into every new
    /// `ReadAuditBuffer` so the buffer's `Drop` can deliver final entries
    /// even if the dispatcher tears down without an explicit flush.
    flush_tx: mpsc::UnboundedSender<FlushMessage>,
    /// Shared counter incremented/decremented by [`ActiveCallGuard`] inside
    /// [`dispatch`]. Same `Arc` instance held by [`WorkerMcpServer`].
    active_delegations: Arc<AtomicU32>,
    /// Per-delegation summary guards. Each entry emits one
    /// `WorkerMcpDelegationSummary` on removal (or on server shutdown).
    delegation_guards:
        Arc<parking_lot::Mutex<std::collections::HashMap<String, DelegationDispatchGuard>>>,
}

/// Token-derived request context cached in middleware and injected into each
/// request as an axum extension so the RMCP handler can build
/// [`WorkerCallContext`] without reading transport-specific headers.
#[derive(Debug, Clone)]
struct AuthenticatedWorkerContext {
    delegation_id: String,
    brain_session_id: String,
}

/// Shared auth state for the axum middleware that gates session creation.
#[derive(Clone)]
struct WorkerAuthMiddlewareState {
    hmac_key: [u8; 32],
    brain_session_id: String,
    session_manager: Arc<LocalSessionManager>,
    session_contexts:
        Arc<parking_lot::Mutex<std::collections::HashMap<String, AuthenticatedWorkerContext>>>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct GetIssueParams {
    id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema, Serialize)]
struct ListIssuesParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority_max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct UpdateIssueParams {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    add_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remove_labels: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct GetPlanStatusParams {
    plan_id: String,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct GetTaskDiffParams {
    plan_id: String,
    task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum OutcomeArtifactSection {
    StatusOnly,
    Summary,
    DiffOnly,
    Full,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct FetchOutcomeArtifactParams {
    delegation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    section: Option<OutcomeArtifactSection>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct CodeSymbolParams {
    symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct CodeSubgraphParams {
    symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    radius: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edge_kinds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct CodeSymbolHistoryParams {
    symbol: String,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct ReportSignalParams {
    task_id: String,
    #[schemars(with = "ReportSignalSchema")]
    signal: Value,
}

#[allow(dead_code)]
#[derive(Debug, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReportSignalKindSchema {
    ScopeDrift,
    RetryExhausted,
}

#[allow(dead_code)]
#[derive(Debug, JsonSchema)]
struct ReportSignalSchema {
    kind: ReportSignalKindSchema,
    signal_id: String,
    #[schemars(range(min = 0.0, max = 1.0))]
    severity: Option<f64>,
    reason: Option<String>,
    #[schemars(range(min = 1))]
    estimated_subtasks: Option<u64>,
    task_id: Option<String>,
    attempt: Option<u32>,
    last_error: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
struct ReportProgressParams {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    percent: Option<f64>,
}

/// RMCP `ServerHandler` for the curated worker tool subset.
struct WorkerToolHandler {
    deps: Arc<DispatcherDeps>,
    brain_session_id: String,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl WorkerToolHandler {
    fn new(deps: Arc<DispatcherDeps>, brain_session_id: String) -> Self {
        Self {
            deps,
            brain_session_id,
            tool_router: Self::tool_router(),
        }
    }

    fn context_from_request(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<WorkerCallContext, McpError> {
        let parts = context
            .extensions
            .get::<axum::http::request::Parts>()
            .ok_or_else(|| {
                McpError::internal_error(
                    "missing HTTP request parts extension for worker auth context",
                    None,
                )
            })?;
        let auth_ctx = parts
            .extensions
            .get::<AuthenticatedWorkerContext>()
            .ok_or_else(|| {
                McpError::new(
                    rmcp::model::ErrorCode(-32001),
                    "unauthorized: missing worker delegation auth context",
                    None,
                )
            })?;
        Ok(WorkerCallContext {
            delegation_id: auth_ctx.delegation_id.clone(),
            brain_session_id: auth_ctx.brain_session_id.clone(),
        })
    }

    async fn invoke_with_lifecycle<F, Fut, E>(
        &self,
        tool_name: &'static str,
        context: RequestContext<RoleServer>,
        read_audit_target: Option<Option<String>>,
        invoke: F,
    ) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(WorkerCallContext) -> Fut,
        Fut: std::future::Future<Output = Result<Value, E>>,
        E: Into<McpError>,
    {
        let worker_ctx = self.context_from_request(&context)?;
        if worker_ctx.brain_session_id != self.brain_session_id {
            return Err(McpError::new(
                rmcp::model::ErrorCode(-32001),
                "unauthorized: token brain_session_id mismatch",
                None,
            ));
        }

        let delegation_id = worker_ctx.delegation_id.clone();
        let _active_guard = ActiveCallGuard::new(Arc::clone(&self.deps.active_delegations));
        let call_start = Instant::now();

        if let Some(target_issue_id) = read_audit_target {
            append_read_audit_entry(&self.deps, &delegation_id, tool_name, target_issue_id);
        }

        let result = invoke(worker_ctx).await;
        let latency_ms = call_start.elapsed().as_millis() as u64;
        let is_error = result.is_err();
        record_call(&self.deps, &delegation_id, tool_name, latency_ms, is_error);

        result.map(CallToolResult::structured).map_err(Into::into)
    }

    #[tool(
        name = "get_issue",
        description = "Retrieve an issue from the configured project management backend (beads, GitHub, etc.).",
        input_schema = crate::tool_schemas::schema_object::<GetIssueParams>()
    )]
    async fn get_issue_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let issue_id = args.get("id").and_then(|v| v.as_str()).map(String::from);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "get_issue",
            context,
            Some(issue_id),
            move |worker_ctx| async move {
                crate::handlers::get_issue(deps.pm_service.as_ref(), &worker_ctx, args).await
            },
        )
        .await
    }

    #[tool(
        name = "list_issues",
        description = "List issues from the configured project management backend with optional filters.",
        input_schema = crate::tool_schemas::schema_object::<ListIssuesParams>()
    )]
    async fn list_issues_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "list_issues",
            context,
            Some(None),
            move |worker_ctx| async move {
                crate::handlers::list_issues(deps.pm_service.as_ref(), &worker_ctx, args).await
            },
        )
        .await
    }

    #[tool(
        name = "get_task_diff",
        description = "Get the full unified diff for a plan task. Use after get_plan_status shows tasks in awaiting_review, approved, rejected, or failed state. Returns the complete diff, worker branch name, task description, and summary for brain code review. Pass `attempt` to inspect prior iteration attempts (see entry.history).",
        input_schema = crate::tool_schemas::schema_object::<GetTaskDiffParams>()
    )]
    async fn get_task_diff_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "get_task_diff",
            context,
            Some(task_id),
            move |worker_ctx| async move {
                crate::handlers::get_task_diff(
                    Some(deps.pm_service.as_ref()),
                    deps.feature_gate.as_ref(),
                    deps.repo_root.as_deref(),
                    deps.plan_resolver.as_ref(),
                    &worker_ctx,
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        name = "get_plan_status",
        description = "Get the current status of a submitted execution plan. Returns per-task status: pending (waiting for deps), ready, dispatched (running), completed, or failed. Non-blocking — returns immediately.",
        input_schema = crate::tool_schemas::schema_object::<GetPlanStatusParams>()
    )]
    async fn get_plan_status_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let plan_id = args
            .get("plan_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "get_plan_status",
            context,
            Some(plan_id),
            move |worker_ctx| async move {
                crate::handlers::get_plan_status(
                    deps.plan_resolver.as_ref(),
                    &deps.reconciler_outcomes,
                    &worker_ctx,
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        name = "fetch_outcome_artifact",
        description = "Fetch the side-channel artifact (full or sectioned) for a completed delegation. Use when continuation.payload.artifact_id is Some(_) and you need fuller context. Sections let you pick what to fetch: pass 'status_only' for just status fields (~100B), 'summary' for the inline summary, 'diff_only' for full diff text, or 'full' for the entire DelegationResult JSON.",
        input_schema = crate::tool_schemas::schema_object::<FetchOutcomeArtifactParams>()
    )]
    async fn fetch_outcome_artifact_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let delegation_id = args
            .get("delegation_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "fetch_outcome_artifact",
            context,
            Some(delegation_id),
            move |worker_ctx| async move {
                if let Some(requested) = args.get("delegation_id").and_then(|v| v.as_str()) {
                    if requested != worker_ctx.delegation_id {
                        return Err(McpHandlerError::Unauthorized(format!(
                            "delegation_id mismatch for bound session context (expected {}, got {requested})",
                            worker_ctx.delegation_id
                        )));
                    }
                }
                crate::handlers::fetch_outcome_artifact(
                    &deps.materializer,
                    deps.outcome_store.as_ref(),
                    &worker_ctx,
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        name = "code_callers",
        description = "List symbols that call the requested code symbol from the current worktree graph artifact. Accepts graph://symbol/<id> URIs or bare stable symbol ids.",
        input_schema = crate::tool_schemas::schema_object::<CodeSymbolParams>()
    )]
    async fn code_callers_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        self.invoke_with_lifecycle(
            "code_callers",
            context,
            Some(None),
            move |_worker_ctx| async move {
                crate::server::handlers::code_graph::code_callers(&args)
            },
        )
        .await
    }

    #[tool(
        name = "code_callees",
        description = "List symbols called by the requested code symbol from the current worktree graph artifact. Accepts graph://symbol/<id> URIs or bare stable symbol ids.",
        input_schema = crate::tool_schemas::schema_object::<CodeSymbolParams>()
    )]
    async fn code_callees_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        self.invoke_with_lifecycle(
            "code_callees",
            context,
            Some(None),
            move |_worker_ctx| async move {
                crate::server::handlers::code_graph::code_callees(&args)
            },
        )
        .await
    }

    #[tool(
        name = "code_subgraph",
        description = "Get a bounded code-symbol subgraph from the current worktree graph artifact. Returns JSON nodes/edges by default, or Mermaid when format=mermaid.",
        input_schema = crate::tool_schemas::schema_object::<CodeSubgraphParams>()
    )]
    async fn code_subgraph_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        self.invoke_with_lifecycle(
            "code_subgraph",
            context,
            Some(None),
            move |_worker_ctx| async move {
                crate::server::handlers::code_graph::code_subgraph(&args)
            },
        )
        .await
    }

    #[tool(
        name = "code_symbol_history",
        description = "Return the causal trace of a code symbol across commits, including ChangeKind and snapshot key for each touch. Requires a temporal commit index in the current worktree.",
        input_schema = crate::tool_schemas::schema_object::<CodeSymbolHistoryParams>()
    )]
    async fn code_symbol_history_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        self.invoke_with_lifecycle(
            "code_symbol_history",
            context,
            Some(None),
            move |_worker_ctx| async move {
                crate::server::handlers::code_graph::code_symbol_history(&args)
            },
        )
        .await
    }

    #[tool(
        name = "update_issue",
        description = "Update an issue in the configured project management backend.",
        input_schema = crate::tool_schemas::schema_object::<UpdateIssueParams>()
    )]
    async fn update_issue_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let issue_id = args.get("id").and_then(|v| v.as_str()).map(String::from);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "update_issue",
            context,
            None,
            move |worker_ctx| async move {
                let result =
                    crate::handlers::update_issue(deps.pm_service.as_ref(), &worker_ctx, args)
                        .await;
                if result.is_ok() {
                    if let Some(issue_id) = issue_id.as_deref() {
                        emit_worker_write_audit(
                            deps.pm_service.as_ref(),
                            deps.feature_gate.as_ref(),
                            &worker_ctx.delegation_id,
                            "update_issue",
                            issue_id,
                        )
                        .await;
                    }
                }
                result
            },
        )
        .await
    }

    #[tool(
        name = "report_signal",
        description = "Worker-facing. Record a typed WorkerSignal on a task. Brain-side watcher will inspect and may mutate the plan.",
        input_schema = crate::tool_schemas::schema_object::<ReportSignalParams>()
    )]
    async fn report_signal_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "report_signal",
            context,
            None,
            move |worker_ctx| async move {
                crate::handlers::report_signal(
                    deps.pm_service.as_ref(),
                    deps.feature_gate.as_ref(),
                    &worker_ctx,
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        name = "report_progress",
        description = "Worker-facing fire-and-forget progress emission. Sends a free-form `message` (and optional `percent`) to the brain as a `WorkerReportProgress` event. The handler returns `{ok: true}` on accept; the side effect IS the event. No PM writes, no audit sentinel — distinct from `report_signal` (which persists). Workers stream rich progress text without minting structured milestone names. Consumers (TUI / dashboards) decide how to render `percent` (no clamping).",
        input_schema = crate::tool_schemas::schema_object::<ReportProgressParams>()
    )]
    async fn report_progress_tool(
        &self,
        arguments: JsonObject,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(arguments);
        let deps = Arc::clone(&self.deps);
        self.invoke_with_lifecycle(
            "report_progress",
            context,
            None,
            move |worker_ctx| async move {
                let delegation_ctx = deps
                    .delegations
                    .lock()
                    .get(&worker_ctx.delegation_id)
                    .copied()
                    .unwrap_or_default();
                if !delegation_ctx.enable_worker_progress {
                    Ok(json!({ "ok": true }))
                } else {
                    crate::handlers::report_progress(deps.funnel.as_ref(), &worker_ctx, args).await
                }
            },
        )
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WorkerToolHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Use these tools to inspect/update assigned issues and report worker progress/signals."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut implementation = Implementation::default();
        implementation.name = "spur-worker-mcp".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }
}

/// Errors returned by [`WorkerMcpServer::start`].
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("worker MCP listener bind failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors returned by [`WorkerMcpServer::flush_delegation`]. The flush is
/// best-effort: a failure here means the audit aggregate could not be
/// queued, but the per-delegation summary funnel event is still emitted.
#[derive(Debug, thiserror::Error)]
pub enum FlushDelegationError {
    /// The background audit flusher's receiver has been dropped (the server
    /// is mid-shutdown). Aggregated read-tool audits queued for this
    /// delegation cannot be persisted to the PM.
    #[error("read-audit flush channel closed (server shutting down)")]
    ChannelClosed,
}

/// Outcome of [`WorkerMcpServer::shutdown`]'s drain phase. `drained` is `true`
/// when every in-flight dispatcher exited before the caller-supplied deadline;
/// `false` when the deadline elapsed with at least one dispatcher still in
/// flight. `active_at_deadline` is the snapshot of [`WorkerMcpServer::active_count`]
/// taken at the moment the drain loop bailed (always `0` when `drained` is
/// `true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownOutcome {
    pub drained: bool,
    pub active_at_deadline: u32,
}

fn unauthorized_response() -> Response<Body> {
    const BODY: &str =
        r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request"},"id":null}"#;
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(BODY))
        .expect("valid unauthorized response")
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let trimmed = value.trim_start();
            if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("Bearer") {
                let rest = trimmed[6..].trim_start();
                if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                }
            } else {
                None
            }
        })
}

fn extract_query_token(uri: &axum::http::Uri) -> Option<String> {
    uri.query().and_then(|query| {
        query.split('&').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            if k == "token" {
                Some(v.to_string())
            } else {
                None
            }
        })
    })
}

async fn worker_auth_middleware(
    State(state): State<WorkerAuthMiddlewareState>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let session_id = request
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    // Requests carrying `Mcp-Session-Id` are trusted ONLY when the id exists
    // in this server's LocalSessionManager and maps to a token-derived
    // delegation context captured at session-open time.
    if let Some(session_id) = session_id {
        let has_session = state
            .session_manager
            .has_session(&SessionId::from(session_id.clone()))
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(
                    ?error,
                    session_id = %session_id,
                    "WorkerMcp AuthDenied: failed to verify session id"
                );
                false
            });
        if has_session {
            let auth_ctx = { state.session_contexts.lock().get(&session_id).cloned() };
            if let Some(auth_ctx) = auth_ctx {
                request.extensions_mut().insert(auth_ctx);
                return next.run(request).await;
            }
            tracing::warn!(
                session_id = %session_id,
                "WorkerMcp AuthDenied: session id exists but has no delegation binding"
            );
            return unauthorized_response();
        }
    }

    let token =
        extract_bearer_token(request.headers()).or_else(|| extract_query_token(request.uri()));
    let token = match token {
        Some(token) => token,
        None => {
            tracing::warn!("WorkerMcp AuthDenied: missing token on session-opening request");
            return unauthorized_response();
        }
    };

    let payload = match validate_token(&state.hmac_key, &token, /*skew_tolerance_secs=*/ 30) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(?error, "WorkerMcp AuthDenied: invalid token");
            return unauthorized_response();
        }
    };

    if payload.b != state.brain_session_id {
        tracing::warn!(
            token_brain_session_id = %payload.b,
            expected_brain_session_id = %state.brain_session_id,
            "WorkerMcp AuthDenied: token brain_session_id mismatch"
        );
        return unauthorized_response();
    }

    let auth_ctx = AuthenticatedWorkerContext {
        delegation_id: payload.d,
        brain_session_id: payload.b,
    };
    request.extensions_mut().insert(auth_ctx.clone());

    let response = next.run(request).await;
    if let Some(session_id) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let has_session = state
            .session_manager
            .has_session(&SessionId::from(session_id.to_string()))
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(
                    ?error,
                    session_id = %session_id,
                    "WorkerMcp AuthDenied: failed to verify minted session id"
                );
                false
            });
        if has_session {
            state
                .session_contexts
                .lock()
                .insert(session_id.to_string(), auth_ctx);
        } else {
            tracing::warn!(
                session_id = %session_id,
                "WorkerMcp AuthDenied: response carried unknown session id"
            );
        }
    }
    response
}

impl WorkerMcpServer {
    /// Bind a fresh TCP listener on `127.0.0.1:0`, generate an in-process HMAC
    /// key from the OS RNG, and spawn the accept loop. Returns once the
    /// listener is bound and ready to accept connections.
    pub async fn start(
        brain_session_id: String,
        deps: WorkerMcpDeps,
    ) -> Result<Arc<Self>, BindError> {
        Self::start_with_config(brain_session_id, deps, WorkerMcpServerConfig::default()).await
    }

    /// Bind a fresh TCP listener on `127.0.0.1:0`, generate an in-process HMAC
    /// key from the OS RNG, spawn the accept loop, and spawn the background
    /// audit flusher task. Returns once the listener is bound and ready to
    /// accept connections.
    pub async fn start_with_config(
        brain_session_id: String,
        deps: WorkerMcpDeps,
        config: WorkerMcpServerConfig,
    ) -> Result<Arc<Self>, BindError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let mut hmac_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut hmac_key);

        let materializer = OutcomeMaterializer::new(Arc::clone(&deps.outcome_store));
        let delegations = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let read_audit_buffers =
            Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushMessage>();
        let active_delegations = Arc::new(AtomicU32::new(0));
        let delegation_guards = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let dispatcher_deps = Arc::new(DispatcherDeps {
            pm_service: deps.pm_service,
            feature_gate: deps.feature_gate,
            funnel: deps.funnel,
            plan_resolver: deps.plan_resolver,
            reconciler_outcomes: deps.reconciler_outcomes,
            outcome_store: deps.outcome_store,
            materializer,
            repo_root: deps.repo_root,
            delegations: Arc::clone(&delegations),
            read_audit_buffers: Arc::clone(&read_audit_buffers),
            flush_tx,
            active_delegations: Arc::clone(&active_delegations),
            delegation_guards: Arc::clone(&delegation_guards),
        });

        let shutdown = CancellationToken::new();
        let session_manager = Arc::new(LocalSessionManager::default());
        let session_contexts = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let server = Arc::new(Self {
            addr,
            hmac_key,
            brain_session_id: brain_session_id.clone(),
            deps: Arc::clone(&dispatcher_deps),
            shutdown: shutdown.clone(),
            session_manager: Arc::clone(&session_manager),
            session_contexts: Arc::clone(&session_contexts),
            accept_loop_handle: Mutex::new(None),
            flusher_handle: Mutex::new(None),
            flush_rx: Mutex::new(Some(flush_rx)),
            active_delegations,
        });

        let handler = Arc::new(WorkerToolHandler::new(
            Arc::clone(&dispatcher_deps),
            brain_session_id.clone(),
        ));
        let service = {
            let handler = Arc::clone(&handler);
            let mut streamable_config = StreamableHttpServerConfig::default();
            streamable_config.stateful_mode = true;
            streamable_config.cancellation_token = shutdown.clone();
            StreamableHttpService::new(
                move || Ok(Arc::clone(&handler)),
                Arc::clone(&session_manager),
                streamable_config,
            )
        };
        let auth_state = WorkerAuthMiddlewareState {
            hmac_key,
            brain_session_id: brain_session_id.clone(),
            session_manager,
            session_contexts,
        };
        let router =
            Router::new()
                .nest_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    auth_state,
                    worker_auth_middleware,
                ));

        let serve_shutdown = shutdown.clone();
        let accept_handle = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(serve_shutdown.cancelled_owned())
                .await
            {
                tracing::debug!(%error, "worker MCP streamable HTTP server exited");
            }
        });
        *server.accept_loop_handle.lock() = Some(accept_handle);

        let flush_rx = server
            .flush_rx
            .lock()
            .take()
            .expect("flush_rx always present at start");
        let flusher_handle = tokio::spawn(audit_flusher_task(
            Arc::clone(&dispatcher_deps),
            flush_rx,
            shutdown.clone(),
            config,
        ));
        *server.flusher_handle.lock() = Some(flusher_handle);

        Ok(server)
    }

    /// The canonical `http://127.0.0.1:<port>/mcp` URL workers POST JSON-RPC
    /// requests to.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// Issue a time-limited bearer token embedding `delegation_id` and this
    /// server's `brain_session_id`. Workers present the token either in the
    /// `Authorization: Bearer <token>` header or as a `?token=<token>` query
    /// parameter.
    pub fn issue_token(&self, delegation_id: &str, ttl: Duration) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload = crate::token::WorkerTokenPayload {
            d: delegation_id.to_string(),
            b: self.brain_session_id.clone(),
            e: now + ttl.as_secs(),
        };
        crate::token::encode_token(&self.hmac_key, &payload)
            .expect("HMAC key length is valid for HmacSha256")
    }

    /// Issue a token with an explicit expiry timestamp (unix seconds).
    /// Primarily useful for tests that need expired tokens.
    #[cfg(any(test, feature = "test-support"))]
    pub fn issue_token_with_expiry(&self, delegation_id: &str, expiry_secs: u64) -> String {
        let payload = crate::token::WorkerTokenPayload {
            d: delegation_id.to_string(),
            b: self.brain_session_id.clone(),
            e: expiry_secs,
        };
        crate::token::encode_token(&self.hmac_key, &payload)
            .expect("HMAC key length is valid for HmacSha256")
    }

    /// Register (or update) cached per-delegation context. Called by the
    /// orchestrator at dispatch time so the `report_progress` dual-gate is
    /// a PM-free HashMap lookup.
    pub fn register_delegation(&self, delegation_id: String, ctx: DelegationContext) {
        let mut map = self.deps.delegations.lock();
        map.insert(delegation_id.clone(), ctx);
        // Ensure a summary guard exists for this delegation.
        let mut guards = self.deps.delegation_guards.lock();
        guards.entry(delegation_id.clone()).or_insert_with(|| {
            DelegationDispatchGuard::new(
                delegation_id,
                self.brain_session_id.clone(),
                Arc::clone(&self.deps.funnel),
            )
        });
    }

    /// Signal that a delegation has reached a terminal state. Removes the
    /// cached context and drops the per-delegation summary guard, which emits
    /// one `WorkerMcpDelegationSummary` event. The `_outcome` parameter is
    /// retained for API stability; per-call error counts in the summary now
    /// come from the dispatcher's `record_call` telemetry.
    pub fn complete_delegation(&self, delegation_id: &str, _outcome: &str) {
        self.deps.delegations.lock().remove(delegation_id);
        self.deps.read_audit_buffers.lock().remove(delegation_id);
        if let Some(guard) = self.deps.delegation_guards.lock().remove(delegation_id) {
            guard.completed.store(true, Ordering::Relaxed);
            // Explicit drop to trigger the summary emission.
            drop(guard);
        }
    }

    /// Phase 5 / Task 27. Drain the per-delegation read-tool audit aggregator
    /// and emit the `WorkerMcpDelegationSummary` event for `delegation_id`.
    /// The orchestrator calls this immediately before emitting
    /// `DelegationCompleted` so downstream consumers observe the audit
    /// summary first.
    ///
    /// Idempotent: a second call (or a call for a delegation that was never
    /// registered, e.g. dispatched without `enable_worker_mcp`) is a no-op
    /// returning `Ok(())`.
    ///
    /// `outcome` is the audit-trail string (`"success"`, `"cancelled"`,
    /// `"rejected"`, or `"error"`). Only `"error"` flips the summary
    /// guard's outcome; the cleaner terminations (`"cancelled"`,
    /// `"rejected"`) preserve the guard's default success bit so the
    /// emitted `WorkerMcpDelegationSummary.outcome` stays `"success"`
    /// for those — but the audit-trail outcome string is propagated to
    /// callers that surface it elsewhere.
    ///
    /// Returns `Err(FlushDelegationError::ChannelClosed)` only when the
    /// background flusher's receiver has been dropped (server shutdown
    /// in progress). The summary event is still emitted in that case;
    /// callers should log the warning, emit a `WorkerMcpSubkind::FlushFailed`
    /// audit if they have an issue context, and continue.
    pub async fn flush_delegation(
        &self,
        delegation_id: &str,
        outcome: &str,
    ) -> Result<(), FlushDelegationError> {
        self.deps.delegations.lock().remove(delegation_id);

        // Drain the read-audit buffer in-band rather than relying on its Drop
        // so that send-failures surface to the caller as a Result.
        let entries = self
            .deps
            .read_audit_buffers
            .lock()
            .remove(delegation_id)
            .map(|buf| buf.take_entries())
            .unwrap_or_default();

        let mut flush_err: Option<FlushDelegationError> = None;
        if !entries.is_empty()
            && self
                .deps
                .flush_tx
                .send(FlushMessage {
                    delegation_id: delegation_id.to_string(),
                    entries,
                })
                .is_err()
        {
            flush_err = Some(FlushDelegationError::ChannelClosed);
        }

        // Drop the per-delegation summary guard regardless of flush outcome.
        // The summary event is part of the contract; losing it would leave
        // the funnel stream missing a sentinel.
        let removed_guard = self.deps.delegation_guards.lock().remove(delegation_id);
        if let Some(guard) = removed_guard {
            if outcome == "error" || flush_err.is_some() {
                guard.mark_error();
            }
            guard.completed.store(true, Ordering::Relaxed);
            drop(guard);
        }

        match flush_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Borrow the read-audit buffer for a delegation. Used by tests to assert
    /// the dispatcher correctly appends per read-tool call. Production callers
    /// (T21 background flusher) reach into `deps.read_audit_buffers` directly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn peek_read_buffer(&self, delegation_id: &str) -> Option<Arc<ReadAuditBuffer>> {
        self.deps
            .read_audit_buffers
            .lock()
            .get(delegation_id)
            .cloned()
    }

    /// Take ownership of the flush-channel receiver. T21's background flusher
    /// calls this once at startup; subsequent calls return `None`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_flush_receiver(&self) -> Option<mpsc::UnboundedReceiver<FlushMessage>> {
        self.flush_rx.lock().take()
    }

    /// Inject a read-audit buffer directly into the map. Used by tests to set
    /// up buffer state without sending HTTP requests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject_read_buffer_for_test(&self, delegation_id: &str) -> Arc<ReadAuditBuffer> {
        let buf = Arc::new(ReadAuditBuffer::new(
            delegation_id.to_string(),
            self.deps.flush_tx.clone(),
        ));
        self.deps
            .read_audit_buffers
            .lock()
            .insert(delegation_id.to_string(), Arc::clone(&buf));
        buf
    }

    /// Number of dispatchers currently in flight. Read by [`shutdown`] to
    /// drain in-flight requests; exposed publicly so callers and tests can
    /// observe pressure without reaching into `DispatcherDeps`.
    pub fn active_count(&self) -> u32 {
        self.active_delegations.load(Ordering::SeqCst)
    }

    /// `true` while the accept loop is still running — i.e. [`shutdown`]
    /// has not been called and the cancellation token has not been
    /// triggered. Read by orchestrator cache code so a stale cached
    /// `Arc<WorkerMcpServer>` (e.g. one whose accept loop was aborted by
    /// a session retire) is evicted and rebooted on next ensure.
    pub fn is_running(&self) -> bool {
        !self.shutdown.is_cancelled()
    }

    async fn close_all_sessions(&self, close_window: Duration) {
        if close_window.is_zero() {
            return;
        }

        let session_ids: Vec<SessionId> = {
            let sessions = self.session_manager.sessions.read().await;
            sessions.keys().cloned().collect()
        };
        if session_ids.is_empty() {
            return;
        }

        let manager = Arc::clone(&self.session_manager);
        let session_contexts = Arc::clone(&self.session_contexts);
        let brain_session_id = self.brain_session_id.clone();
        let close_sessions = async move {
            for session_id in session_ids {
                if let Err(error) = manager.close_session(&session_id).await {
                    tracing::warn!(
                        %error,
                        session_id = %session_id,
                        brain_session_id = %brain_session_id,
                        "WorkerMcpServer close_session failed during shutdown drain"
                    );
                }
                session_contexts.lock().remove(session_id.as_ref());
            }
        };

        if tokio::time::timeout(close_window, close_sessions)
            .await
            .is_err()
        {
            tracing::warn!(
                brain_session_id = %self.brain_session_id,
                timeout_ms = close_window.as_millis() as u64,
                "WorkerMcpServer timed out while closing active sessions"
            );
        }
    }

    /// Cancel the accept loop, drain in-flight dispatchers up to `deadline`,
    /// then join the background audit flusher task. Returns a
    /// [`ShutdownOutcome`] describing whether the drain completed (`drained =
    /// true`) or the deadline elapsed with dispatchers still in flight
    /// (`drained = false`, `active_at_deadline > 0`).
    ///
    /// Drain semantics:
    /// 1. The accept loop is cancelled first, so the listener stops accepting
    ///    new connections immediately.
    /// 2. Active RMCP sessions are closed so long-lived SSE streams are
    ///    proactively terminated.
    /// 3. Existing dispatchers continue and decrement `active_delegations`
    ///    via [`ActiveCallGuard::drop`] as they return.
    /// 4. The drain loop polls [`active_count`](Self::active_count) every
    ///    `DRAIN_POLL_INTERVAL` (bounded — never busy-loops) until either the
    ///    counter reaches `0` or `deadline` elapses.
    /// 5. On deadline elapse a `WARN`-level log is emitted with the in-flight
    ///    count. The flusher task is still joined so its TaskTracker drains
    ///    and the deps `Arc` is released.
    ///
    /// The caller picks `deadline`; a typical default is `5s`. Callers that
    /// don't care about the outcome may discard the return value.
    /// Idempotent: a second call after shutdown finds the accept and flusher
    /// handles already taken and returns immediately with `drained = true`.
    pub async fn shutdown(self: Arc<Self>, deadline: Duration) -> ShutdownOutcome {
        let drain_deadline = Instant::now() + deadline;
        self.shutdown.cancel();
        self.close_all_sessions(deadline).await;

        // Listener cancellation + session close-all prevents new work and
        // terminates active SSE streams. Drain in-flight dispatchers, bounded
        // by `deadline`.
        let outcome = loop {
            let active = self.active_count();
            if active == 0 {
                break ShutdownOutcome {
                    drained: true,
                    active_at_deadline: 0,
                };
            }
            if Instant::now() >= drain_deadline {
                tracing::warn!(
                    brain_session_id = %self.brain_session_id,
                    active = active,
                    deadline_ms = deadline.as_millis() as u64,
                    "WorkerMcpServer drain deadline elapsed with in-flight dispatchers"
                );
                break ShutdownOutcome {
                    drained: false,
                    active_at_deadline: active,
                };
            }
            tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
        };

        let accept_handle = self.accept_loop_handle.lock().take();
        if let Some(handle) = accept_handle {
            let wait = drain_deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                handle.abort();
            } else {
                let _ = tokio::time::timeout(wait, handle).await;
            }
        }

        let flusher_handle = self.flusher_handle.lock().take();
        if let Some(handle) = flusher_handle {
            let wait = std::cmp::min(
                Duration::from_secs(1),
                drain_deadline.saturating_duration_since(Instant::now()),
            );
            if wait.is_zero() {
                handle.abort();
            } else {
                let _ = tokio::time::timeout(wait, handle).await;
            }
        }
        // Reference held only to keep the deps Arc alive until shutdown
        // completes; explicit drop documents intent.
        drop(self.deps.clone());
        outcome
    }
}

/// How often [`WorkerMcpServer::shutdown`] re-checks `active_count` while
/// draining. Bounded so the polling loop never busy-spins, small enough that
/// a fast-completing dispatcher doesn't materially extend shutdown latency.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Append a read-tool call to the per-delegation aggregation buffer. Lazily
/// creates the buffer on first call. Lock scope is intentionally tight — the
/// outer `read_audit_buffers` mutex is released before any `append` so a slow
/// `Drop` (channel send) never blocks an unrelated delegation.
fn append_read_audit_entry(
    deps: &DispatcherDeps,
    delegation_id: &str,
    tool_name: &str,
    target_issue_id: Option<String>,
) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let buf = {
        let mut map = deps.read_audit_buffers.lock();
        Arc::clone(map.entry(delegation_id.to_string()).or_insert_with(|| {
            Arc::new(ReadAuditBuffer::new(
                delegation_id.to_string(),
                deps.flush_tx.clone(),
            ))
        }))
    };
    buf.append(ReadAuditEntry {
        tool_name: tool_name.to_string(),
        target_issue_id,
        ts,
    });
}

/// Record one completed tool call on the per-delegation summary guard. No-op
/// if the guard has not been created yet (e.g., unregistered delegation).
fn record_call(
    deps: &DispatcherDeps,
    delegation_id: &str,
    tool_name: &str,
    latency_ms: u64,
    is_error: bool,
) {
    let map = deps.delegation_guards.lock();
    if let Some(guard) = map.get(delegation_id) {
        guard.record_call(tool_name, latency_ms, is_error);
    }
}

/// Maximum time to wait for an audit sentinel comment to be written before
/// giving up and returning success to the worker. Slow beads must not stall
/// the worker indefinitely.
const AUDIT_EMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn a dedicated background task that owns the `flush_rx` half of the
/// read-audit flush channel. The task receives `FlushMessage`s (either from
/// buffer `Drop` or from the periodic idle scan) and writes aggregated
/// `ReadAggregate` sentinel comments to the PM backend. It also scans the
/// `read_audit_buffers` map every `scan_interval` and removes idle entries
/// (last entry `ts` older than `idle_threshold`).
async fn audit_flusher_task(
    deps: Arc<DispatcherDeps>,
    mut flush_rx: mpsc::UnboundedReceiver<FlushMessage>,
    shutdown: CancellationToken,
    config: WorkerMcpServerConfig,
) {
    let mut interval = tokio::time::interval(config.scan_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(msg) = flush_rx.recv() => {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    _ = emit_read_aggregate(&deps, msg.delegation_id, msg.entries, &shutdown) => {},
                }
            }
            _ = interval.tick() => {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    _ = scan_and_flush_idle_buffers(&deps, &config, &shutdown) => {},
                }
            }
        }
    }
}

/// Scan the per-delegation read-audit buffer map and flush idle buffers.
///
/// Concurrency note: while the map lock is held, idle buffers are removed
/// and their entries drained. If a concurrent handler holds an `Arc` clone
/// and appends AFTER drain, the entry remains in the buffer until the
/// handler returns; `Drop` then sends a follow-up `FlushMessage`. This
/// produces a duplicate sentinel comment but no data loss in steady state.
///
/// TODO(T24): During shutdown, the flusher's recv loop exits before all
/// in-flight `Arc` clones have dropped. Drop-fired `FlushMessage`s after that
/// point are silently lost. T24's shutdown drain owns this window.
async fn scan_and_flush_idle_buffers(
    deps: &DispatcherDeps,
    config: &WorkerMcpServerConfig,
    shutdown: &CancellationToken,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let threshold = config.idle_threshold;

    let to_flush: Vec<(String, Vec<ReadAuditEntry>)> = {
        let mut map = deps.read_audit_buffers.lock();
        let mut result = Vec::new();

        let idle_ids: Vec<String> = map
            .iter()
            .filter(|(_, buf)| {
                let entries = buf.entries.lock();
                match entries.last() {
                    Some(e) => {
                        let entry_time = Duration::from_secs(e.ts);
                        now.saturating_sub(entry_time) > threshold
                    }
                    None => true,
                }
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in idle_ids {
            if let Some(buf) = map.remove(&id) {
                let entries = std::mem::take(&mut *buf.entries.lock());
                if !entries.is_empty() {
                    result.push((id, entries));
                }
            }
        }

        result
    };

    for (delegation_id, entries) in to_flush {
        if shutdown.is_cancelled() {
            break;
        }
        emit_read_aggregate(deps, delegation_id, entries, shutdown).await;
    }
}

/// Encode a `ReadAggregate` sentinel from the drained entries and write it
/// to the PM backend. The comment is placed on the first non-None
/// `target_issue_id` in the entry list; if none exists the flush is a no-op.
/// The flusher task remains cancellable via `shutdown` so it can exit promptly
/// even if PM I/O is slow.
async fn emit_read_aggregate(
    deps: &DispatcherDeps,
    delegation_id: String,
    entries: Vec<ReadAuditEntry>,
    shutdown: &CancellationToken,
) {
    if entries.is_empty() {
        return;
    }

    let issue_id = match entries.iter().find_map(|e| e.target_issue_id.clone()) {
        Some(id) => id,
        None => {
            tracing::warn!(
                delegation_id = %delegation_id,
                entry_count = entries.len(),
                tools = ?entries.iter().map(|e| &e.tool_name).collect::<Vec<_>>(),
                "ReadAggregate audit dropped: no entry has a target_issue_id (all read-only ops without explicit target)"
            );
            return;
        }
    };

    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        &deps.feature_gate,
    ) {
        tracing::warn!(
            delegation_id = %delegation_id,
            issue_id = %issue_id,
            "ReadAggregate audit comment emission skipped: {error:?}"
        );
        return;
    }

    let kind = crate::plan::audit_sentinel::AuditSentinelKind::ReadAggregate {
        delegation_id,
        entries: entries
            .into_iter()
            .map(|e| crate::plan::audit_sentinel::ReadAggregateEntry {
                tool_name: e.tool_name,
                target_issue_id: e.target_issue_id,
                ts: e.ts,
            })
            .collect(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);

    let Some(adv) = deps.pm_service.advanced() else {
        return;
    };

    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {}
        _ = adv.add_comment(&issue_id, &body) => {}
    }
}

/// Emit a `[[spur-audit v1]] WorkerWrite` sentinel comment on the issue that
/// was just mutated by a worker write tool. Called synchronously before the
/// handler result is returned to the worker so the audit trail is durable even
/// if the worker process dies immediately after the call.
async fn emit_worker_write_audit(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    delegation_id: &str,
    tool: &str,
    issue_id: &str,
) {
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    ) {
        tracing::warn!(
            delegation_id = %delegation_id,
            issue_id = %issue_id,
            tool = %tool,
            "WorkerWrite audit comment emission skipped: {error:?}"
        );
        return;
    }
    emit_worker_write_audit_inner(pm.advanced(), delegation_id, tool, issue_id).await;
}

async fn emit_worker_write_audit_inner(
    advanced: Option<&dyn spur_pm::BeadsAdvanced>,
    delegation_id: &str,
    tool: &str,
    issue_id: &str,
) {
    let Some(adv) = advanced else {
        return;
    };
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::WorkerWrite {
        delegation_id: delegation_id.to_string(),
        tool: tool.to_string(),
        issue_id: issue_id.to_string(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    let emit_fut = adv.add_comment(issue_id, &body);
    match tokio::time::timeout(AUDIT_EMIT_TIMEOUT, emit_fut).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                delegation_id = %delegation_id,
                issue_id = %issue_id,
                tool = %tool,
                "WorkerWrite audit comment emission failed: {e}"
            );
        }
        Err(_) => {
            tracing::warn!(
                delegation_id = %delegation_id,
                issue_id = %issue_id,
                tool = %tool,
                "WorkerWrite audit comment emission timed out after {}s",
                AUDIT_EMIT_TIMEOUT.as_secs()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use spur_pm::{BeadsAdvanced, Comment, CommentId, DependencyCycle, IssueSummary, ReadyFilter};
    use tracing::field::{Field, Visit};

    use super::*;

    // ─── Mock BeadsAdvanced implementations ───────────────────────────────

    struct RecordingAdvanced {
        comments: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl BeadsAdvanced for RecordingAdvanced {
        async fn list_ready(&self, _filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
            Ok(Vec::new())
        }
        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<Comment>> {
            Ok(Vec::new())
        }
        async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId> {
            self.comments
                .lock()
                .unwrap()
                .push((issue_id.to_string(), body.to_string()));
            Ok("c-1".into())
        }
        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
            Ok(Vec::new())
        }
    }

    struct FailingAdvanced;

    #[async_trait]
    impl BeadsAdvanced for FailingAdvanced {
        async fn list_ready(&self, _filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
            Ok(Vec::new())
        }
        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<Comment>> {
            Ok(Vec::new())
        }
        async fn add_comment(&self, _issue_id: &str, _body: &str) -> anyhow::Result<CommentId> {
            Err(anyhow::anyhow!("simulated add_comment failure"))
        }
        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
            Ok(Vec::new())
        }
    }

    struct HangingAdvanced;

    #[async_trait]
    impl BeadsAdvanced for HangingAdvanced {
        async fn list_ready(&self, _filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
            Ok(Vec::new())
        }
        async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<Comment>> {
            Ok(Vec::new())
        }
        async fn add_comment(&self, _issue_id: &str, _body: &str) -> anyhow::Result<CommentId> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("c-1".into())
        }
        async fn remove_dependency(
            &self,
            _issue_id: &str,
            _depends_on_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
            Ok(Vec::new())
        }
    }

    // ─── Warning capture helper ───────────────────────────────────────────

    #[derive(Clone, Default)]
    struct CapturedWarnings {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl CapturedWarnings {
        fn contains(&self, needle: &str) -> bool {
            self.events
                .lock()
                .unwrap()
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
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(visitor.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct StringVisitor(String);

    impl Visit for StringVisitor {
        fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!("{value:?}"));
        }
    }

    // ─── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn emit_worker_write_audit_inner_writes_sentinel_comment() {
        let recorder = RecordingAdvanced {
            comments: Mutex::new(Vec::new()),
        };
        emit_worker_write_audit_inner(Some(&recorder), "del-A", "update_issue", "bd-123").await;

        let comments = recorder.comments.lock().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].0, "bd-123");
        assert!(comments[0]
            .1
            .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX));
        assert!(comments[0].1.contains("\"kind\":\"worker-write\""));
        assert!(comments[0].1.contains("\"delegation_id\":\"del-A\""));
        assert!(comments[0].1.contains("\"tool\":\"update_issue\""));
        assert!(comments[0].1.contains("\"issue_id\":\"bd-123\""));
    }

    #[tokio::test]
    async fn emit_worker_write_audit_inner_logs_warning_on_failure() {
        let warnings = CapturedWarnings::default();
        let _guard = tracing::subscriber::set_default(warnings.clone());

        emit_worker_write_audit_inner(Some(&FailingAdvanced), "del-A", "update_issue", "bd-123")
            .await;

        assert!(
            warnings.contains("WorkerWrite audit comment emission failed"),
            "expected warning about audit emission failure, got: {:?}",
            warnings.events.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn emit_worker_write_audit_inner_noop_when_advanced_none() {
        // Should not panic and should complete immediately.
        emit_worker_write_audit_inner(None, "del-A", "update_issue", "bd-123").await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn emit_worker_write_audit_inner_times_out_and_warns() {
        let warnings = CapturedWarnings::default();

        let handle = tokio::spawn({
            let warnings = warnings.clone();
            async move {
                let _guard = tracing::subscriber::set_default(warnings);
                let hanging = HangingAdvanced;
                emit_worker_write_audit_inner(Some(&hanging), "del-A", "update_issue", "bd-123")
                    .await;
            }
        });

        tokio::time::advance(Duration::from_secs(6)).await;
        handle.await.expect("emit completes");

        assert!(
            warnings.contains("timed out after 5s"),
            "expected timeout warning, got: {:?}",
            warnings.events.lock().unwrap()
        );
    }

    #[test]
    fn delegation_dispatch_guard_drop_emits_summary_via_try_emit() {
        use std::sync::atomic::AtomicBool;

        struct TryEmitRecordingSink {
            events: Mutex<Vec<spur_acp::SpurEventBody>>,
            full: AtomicBool,
        }

        impl crate::events::McpEventSink for TryEmitRecordingSink {
            fn emit(&self, _event: spur_acp::SpurEventBody) {
                panic!("emit must not be called — try_emit should be used");
            }

            fn try_emit(
                &self,
                event: spur_acp::SpurEventBody,
            ) -> Result<(), spur_acp::SpurEventBody> {
                if self.full.load(Ordering::SeqCst) {
                    return Err(event);
                }
                self.events.lock().unwrap().push(event);
                Ok(())
            }
        }

        let sink = Arc::new(TryEmitRecordingSink {
            events: Mutex::new(Vec::new()),
            full: AtomicBool::new(false),
        });

        let guard = DelegationDispatchGuard::new(
            "d-test".into(),
            "session-test".into(),
            Arc::clone(&sink) as Arc<dyn crate::events::McpEventSink>,
        );
        guard.record_call("get_issue", 10, false);
        guard.record_call("get_issue", 20, false);
        guard.record_call("update_issue", 30, true);
        guard.completed.store(true, Ordering::Relaxed);
        drop(guard);

        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        if let spur_acp::SpurEventBody::WorkerMcpDelegationSummary {
            delegation_id,
            brain_session_id,
            calls_total,
            calls_by_tool,
            errors,
            ..
        } = &events[0]
        {
            assert_eq!(delegation_id, "d-test");
            assert_eq!(brain_session_id, "session-test");
            assert_eq!(*calls_total, 3);
            assert_eq!(calls_by_tool.get("get_issue"), Some(&2));
            assert_eq!(calls_by_tool.get("update_issue"), Some(&1));
            assert_eq!(*errors, 1);
        } else {
            panic!("expected WorkerMcpDelegationSummary, got: {:?}", events[0]);
        }

        // When the sink is full, the guard should silently drop the event.
        let sink2 = Arc::new(TryEmitRecordingSink {
            events: Mutex::new(Vec::new()),
            full: AtomicBool::new(true),
        });
        let guard2 = DelegationDispatchGuard::new(
            "d-full".into(),
            "session-full".into(),
            Arc::clone(&sink2) as Arc<dyn crate::events::McpEventSink>,
        );
        guard2.record_call("get_issue", 5, false);
        guard2.completed.store(true, Ordering::Relaxed);
        drop(guard2);
        assert!(
            sink2.events.lock().unwrap().is_empty(),
            "event must be dropped when sink is full"
        );
    }

    #[test]
    fn compute_p99_latency_ms_matches_spec() {
        // Empty: 0
        assert_eq!(compute_p99_latency_ms(&[]), 0);
        // Single sample: max observed.
        assert_eq!(compute_p99_latency_ms(&[42]), 42);
        // <2 was already covered; for n>=2 use nearest-rank.
        // Two samples: ceil(0.99*2) = 2 → idx 1 → max.
        assert_eq!(compute_p99_latency_ms(&[10, 100]), 100);
        // 100 evenly-spaced samples 1..=100: ceil(0.99*100) = 99 → idx 98 → 99.
        let samples: Vec<u64> = (1..=100).collect();
        assert_eq!(compute_p99_latency_ms(&samples), 99);
        // Unsorted input: still uses sorted nearest-rank.
        let unsorted: Vec<u64> = vec![50, 5, 200, 100, 25];
        // Sorted: [5,25,50,100,200]; ceil(0.99*5) = 5 → idx 4 → 200.
        assert_eq!(compute_p99_latency_ms(&unsorted), 200);
    }
}
