//! Per-`BrainSession` HTTP/JSON-RPC server exposing the curated worker MCP
//! tool subset to delegated workers.
//!
//! T15 implements the lifecycle skeleton: bind a TCP listener on
//! `127.0.0.1:0`, run a cancellable accept loop, and expose `start()` /
//! `url()` / `shutdown()`. T16 adds HMAC token validation middleware.
//! T17 wires the JSON-RPC dispatcher: `tools/list` returns the curated
//! 8-tool subset and `tools/call` routes to the freestanding handlers in
//! [`crate::handlers`]. Audit emission (T19+) and per-delegation gating
//! (T18) layer on top of this dispatcher.

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
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::McpEventSink;
use crate::handlers::{McpHandlerError, PlanResolver, WorkerCallContext};
use crate::outcome_materializer::OutcomeMaterializer;
use crate::token::{validate_token, TokenError};

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

/// Maximum allowed HTTP body size (1 MiB). JSON-RPC payloads are tiny;
/// anything larger is treated as an attack.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Timeout for reading the request line.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for reading the request body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for the entire headers-parsing phase. Prevents slowloris-style
/// attacks that trickle one short header at a time to stay under the
/// per-iteration `READ_TIMEOUT`.
const HEADERS_PHASE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum length of a single header line.
const MAX_HEADER_LINE: usize = 8 * 1024;

/// Maximum number of headers accepted per request.
const MAX_HEADERS: usize = 64;

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
        let server = Arc::new(Self {
            addr,
            hmac_key,
            brain_session_id: brain_session_id.clone(),
            deps: Arc::clone(&dispatcher_deps),
            shutdown: shutdown.clone(),
            accept_loop_handle: Mutex::new(None),
            flusher_handle: Mutex::new(None),
            flush_rx: Mutex::new(Some(flush_rx)),
            active_delegations,
        });

        let accept_handle = tokio::spawn(accept_loop(
            listener,
            shutdown.clone(),
            hmac_key,
            brain_session_id.clone(),
            Arc::clone(&dispatcher_deps),
        ));
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

    /// Cancel the accept loop, drain in-flight dispatchers up to `deadline`,
    /// then join the background audit flusher task. Returns a
    /// [`ShutdownOutcome`] describing whether the drain completed (`drained =
    /// true`) or the deadline elapsed with dispatchers still in flight
    /// (`drained = false`, `active_at_deadline > 0`).
    ///
    /// Drain semantics:
    /// 1. The accept loop is cancelled first, so the listener stops accepting
    ///    new connections immediately.
    /// 2. Existing dispatchers continue and decrement `active_delegations`
    ///    via [`ActiveCallGuard::drop`] as they return.
    /// 3. The drain loop polls [`active_count`](Self::active_count) every
    ///    `DRAIN_POLL_INTERVAL` (bounded — never busy-loops) until either the
    ///    counter reaches `0` or `deadline` elapses.
    /// 4. On deadline elapse a `WARN`-level log is emitted with the in-flight
    ///    count. The flusher task is still joined so its TaskTracker drains
    ///    and the deps `Arc` is released.
    ///
    /// The caller picks `deadline`; a typical default is `5s`. Callers that
    /// don't care about the outcome may discard the return value.
    /// Idempotent: a second call after shutdown finds the accept and flusher
    /// handles already taken and returns immediately with `drained = true`.
    pub async fn shutdown(self: Arc<Self>, deadline: Duration) -> ShutdownOutcome {
        self.shutdown.cancel();
        let accept_handle = self.accept_loop_handle.lock().take();
        if let Some(handle) = accept_handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        // Listener is closed (accept_loop has returned), so no new requests
        // can arrive. Drain existing dispatchers, bounded by `deadline`.
        let drain_deadline = Instant::now() + deadline;
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

        let flusher_handle = self.flusher_handle.lock().take();
        if let Some(handle) = flusher_handle {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
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

/// Free fn so the spawned task does not capture an `Arc<Self>` — that would
/// create a strong reference cycle with `accept_loop_handle` and prevent the
/// server from ever dropping if a caller forgets to call `shutdown()`.
async fn accept_loop(
    listener: TcpListener,
    shutdown: CancellationToken,
    hmac_key: [u8; 32],
    brain_session_id: String,
    deps: Arc<DispatcherDeps>,
) {
    // TODO(T24): track per-connection tasks via tokio_util::task::TaskTracker
    // so shutdown drains in-flight requests instead of detaching them.
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            accept = listener.accept() => match accept {
                Ok((stream, peer)) => {
                    tokio::spawn(handle_connection(
                        stream,
                        peer,
                        hmac_key,
                        brain_session_id.clone(),
                        Arc::clone(&deps),
                    ));
                }
                Err(_) => {
                    // Avoid pegging a core at 100% on persistent OS errors
                    // such as EMFILE — a short backoff lets the runtime
                    // recover and keeps cancellation responsive.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

/// Parse an HTTP request, validate the bearer token, and either reject with
/// 401 or dispatch to the JSON-RPC handler.
async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    hmac_key: [u8; 32],
    brain_session_id: String,
    deps: Arc<DispatcherDeps>,
) {
    let response = match handle_request(&mut stream, &hmac_key, &brain_session_id, deps).await {
        Ok(body) => format_http_response(200, "OK", &body),
        Err(err) => {
            tracing::warn!(?peer, error = ?err, "WorkerMcp AuthDenied");
            let body = r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request"},"id":null}"#;
            format_http_response(401, "Unauthorized", body)
        }
    };

    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Format a minimal HTTP/1.1 response with a JSON body.
fn format_http_response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Extract the bearer token from the `Authorization` header (case-insensitive
/// scheme) or the `?token=` query string.
fn extract_token(request_line: &str, headers: &[(String, String)]) -> Option<String> {
    // 1. Prefer Authorization: Bearer <token> header.
    let from_header = headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .and_then(|(_, v)| {
            let trimmed = v.trim_start();
            if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("Bearer") {
                let rest = trimmed[6..].trim_start();
                Some(rest.to_string())
            } else {
                None
            }
        });

    if from_header.is_some() {
        return from_header;
    }

    // 2. Fall back to ?token=<token> query parameter.
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let path_and_query = parts[1];
    path_and_query.split_once('?').and_then(|(_, query)| {
        query.split('&').find_map(|param| {
            let (k, v) = param.split_once('=')?;
            if k == "token" {
                Some(v.to_string())
            } else {
                None
            }
        })
    })
}

/// Read and parse an HTTP request from `stream`, validate the bearer token,
/// and on success return the JSON-RPC body string. On any failure returns a
/// `TokenError` which is logged (without token bytes) and mapped to HTTP 401.
async fn handle_request(
    stream: &mut TcpStream,
    hmac_key: &[u8; 32],
    brain_session_id: &str,
    deps: Arc<DispatcherDeps>,
) -> Result<String, TokenError> {
    let mut buf_reader = tokio::io::BufReader::new(stream);
    let mut request_line = String::new();

    let mut limited = (&mut buf_reader).take(MAX_HEADER_LINE as u64);
    tokio::time::timeout(READ_TIMEOUT, limited.read_line(&mut request_line))
        .await
        .map_err(|_| TokenError::Malformed)?
        .map_err(|_| TokenError::Malformed)?;

    if !request_line.ends_with('\n') {
        return Err(TokenError::Malformed);
    }

    let headers = tokio::time::timeout(HEADERS_PHASE_TIMEOUT, async {
        let mut headers = Vec::new();
        loop {
            if headers.len() >= MAX_HEADERS {
                return Err(TokenError::Malformed);
            }
            let mut line = String::new();
            let mut limited = (&mut buf_reader).take(MAX_HEADER_LINE as u64);
            tokio::time::timeout(READ_TIMEOUT, limited.read_line(&mut line))
                .await
                .map_err(|_| TokenError::Malformed)?
                .map_err(|_| TokenError::Malformed)?;
            if !line.ends_with('\n') {
                return Err(TokenError::Malformed);
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((k, v)) = line.trim_end().split_once(':') {
                headers.push((k.to_lowercase(), v.trim().to_string()));
            }
        }
        Ok::<_, TokenError>(headers)
    })
    .await
    .map_err(|_| TokenError::Malformed)??;

    let content_length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > MAX_BODY_BYTES {
        return Err(TokenError::Malformed);
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        tokio::time::timeout(BODY_READ_TIMEOUT, buf_reader.read_exact(&mut body))
            .await
            .map_err(|_| TokenError::Malformed)?
            .map_err(|_| TokenError::Malformed)?;
    }

    // Drop buf_reader so the caller can write to the underlying stream.
    drop(buf_reader);

    let token = extract_token(&request_line, &headers).ok_or(TokenError::Malformed)?;
    let payload = validate_token(hmac_key, &token, /*skew_tolerance_secs=*/ 30)?;

    if payload.b != brain_session_id {
        return Err(TokenError::BadSignature);
    }

    let ctx = WorkerCallContext {
        delegation_id: payload.d,
        brain_session_id: payload.b,
    };

    Ok(dispatch(ctx, body, deps).await)
}

/// JSON-RPC dispatcher. Returns the serialized response body — even error
/// responses are HTTP 200 once the token has been validated, per JSON-RPC 2.0
/// (errors live inside the body, not in the transport status).
///
/// Reads the request body, rejects batches (`-32600`), then routes:
/// - `tools/list` → curated 8-tool subset from [`crate::tools::worker_tools_list`]
/// - `tools/call` → freestanding handler in [`crate::handlers`] keyed by name
///
/// Unknown method or unknown tool name → `-32601 Method not found`.
async fn dispatch(ctx: WorkerCallContext, body: Vec<u8>, deps: Arc<DispatcherDeps>) -> String {
    // RAII counter — held for the entire dispatcher lifetime including
    // panics and early returns. Drop fires `fetch_sub` so `active_count`
    // returns to 0 once every in-flight request finishes. `shutdown()`
    // polls this counter to know when it is safe to tear down.
    let _active_guard = ActiveCallGuard::new(Arc::clone(&deps.active_delegations));

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return error_response(Value::Null, -32700, "Parse error"),
    };

    // Batched JSON-RPC requests would force per-element token re-validation
    // and audit attribution we deliberately don't support — reject up front.
    if parsed.is_array() {
        return error_response(Value::Null, -32600, "Batched requests are not supported");
    }

    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let method = parsed
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = parsed.get("params").cloned().unwrap_or_else(|| json!({}));

    match method.as_str() {
        "initialize" => {
            // Provide a bare-minimum MCP initialization response.
            // This is required for real worker clients to establish the connection
            // before they invoke tools/list.
            success_response(
                id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "spur-worker-mcp",
                        "version": "1.0.0"
                    }
                }),
            )
        }
        "notifications/initialized" => {
            // Client acknowledges initialization. No response needed for notifications.
            String::new()
        }
        "tools/list" => {
            let tools = crate::tools::worker_tools_list();
            success_response(id, json!({ "tools": tools }))
        }
        "tools/call" => dispatch_tool_call(ctx, id, params, deps).await,
        other => error_response(id, -32601, format!("Method not found: {other}")),
    }
}

async fn dispatch_tool_call(
    ctx: WorkerCallContext,
    id: Value,
    params: Value,
    deps: Arc<DispatcherDeps>,
) -> String {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return error_response(id, -32602, "missing required field 'name'"),
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let call_start = Instant::now();

    let result: Result<Value, McpHandlerError> = match name.as_str() {
        "get_issue" => {
            append_read_audit_entry(
                deps.as_ref(),
                &ctx.delegation_id,
                "get_issue",
                args.get("id").and_then(|v| v.as_str()).map(String::from),
            );
            crate::handlers::get_issue(deps.pm_service.as_ref(), &ctx, args).await
        }
        "list_issues" => {
            append_read_audit_entry(deps.as_ref(), &ctx.delegation_id, "list_issues", None);
            crate::handlers::list_issues(deps.pm_service.as_ref(), &ctx, args).await
        }
        "update_issue" => {
            let issue_id = args.get("id").and_then(|v| v.as_str()).map(String::from);
            let result = crate::handlers::update_issue(deps.pm_service.as_ref(), &ctx, args).await;
            if result.is_ok() {
                if let Some(ref issue_id) = issue_id {
                    emit_worker_write_audit(
                        deps.pm_service.as_ref(),
                        deps.feature_gate.as_ref(),
                        &ctx.delegation_id,
                        "update_issue",
                        issue_id,
                    )
                    .await;
                }
            }
            result
        }
        "report_signal" => {
            crate::handlers::report_signal(
                deps.pm_service.as_ref(),
                deps.feature_gate.as_ref(),
                &ctx,
                args,
            )
            .await
        }
        "report_progress" => {
            // Dual-gate gate 1: check cached delegation context. If progress
            // is disabled, silently drop and return success (fire-and-forget).
            let delegation_ctx = deps
                .delegations
                .lock()
                .get(&ctx.delegation_id)
                .copied()
                .unwrap_or_default();
            if !delegation_ctx.enable_worker_progress {
                Ok(json!({ "ok": true }))
            } else {
                crate::handlers::report_progress(deps.funnel.as_ref(), &ctx, args).await
            }
        }
        "get_plan_status" => {
            append_read_audit_entry(
                deps.as_ref(),
                &ctx.delegation_id,
                "get_plan_status",
                args.get("plan_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            crate::handlers::get_plan_status(
                deps.plan_resolver.as_ref(),
                &deps.reconciler_outcomes,
                &ctx,
                args,
            )
            .await
        }
        "fetch_outcome_artifact" => {
            append_read_audit_entry(
                deps.as_ref(),
                &ctx.delegation_id,
                "fetch_outcome_artifact",
                args.get("delegation_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            crate::handlers::fetch_outcome_artifact(
                &deps.materializer,
                deps.outcome_store.as_ref(),
                &ctx,
                args,
            )
            .await
        }
        "get_task_diff" => {
            append_read_audit_entry(
                deps.as_ref(),
                &ctx.delegation_id,
                "get_task_diff",
                args.get("task_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            );
            crate::handlers::get_task_diff(
                Some(deps.pm_service.as_ref()),
                deps.feature_gate.as_ref(),
                deps.repo_root.as_deref(),
                deps.plan_resolver.as_ref(),
                &ctx,
                args,
            )
            .await
        }
        other => {
            // Unknown tool name: record as an errored call so the summary
            // event reflects the dispatch attempt.
            let latency_ms = call_start.elapsed().as_millis() as u64;
            record_call(&deps, &ctx.delegation_id, &name, latency_ms, true);
            return error_response(id, -32601, format!("Method not found: {other}"));
        }
    };

    let latency_ms = call_start.elapsed().as_millis() as u64;
    let is_error = result.is_err();
    record_call(&deps, &ctx.delegation_id, &name, latency_ms, is_error);

    match result {
        Ok(value) => success_response(id, value),
        Err(err) => {
            let resp = err.to_jsonrpc_response(id);
            serde_json::to_string(&resp).unwrap_or_else(|_| {
                r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"response serialization failed"},"id":null}"#.to_string()
            })
        }
    }
}

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

fn success_response(id: Value, result: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"response serialization failed"},"id":null}"#.to_string()
    })
}

fn error_response(id: Value, code: i32, message: impl Into<String>) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    }))
    .unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"response serialization failed"},"id":null}"#.to_string()
    })
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
