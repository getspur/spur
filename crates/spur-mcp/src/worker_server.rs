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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
}

/// Errors returned by [`WorkerMcpServer::start`].
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("worker MCP listener bind failed: {0}")]
    Io(#[from] std::io::Error),
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
        self.deps.delegations.lock().insert(delegation_id, ctx);
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

    /// Cancel the accept loop and wait up to 5 seconds for it to exit. Then
    /// drain in-flight dispatchers (poll [`active_count`](Self::active_count)
    /// until it reaches 0) before joining the background audit flusher task.
    /// T22 introduces the unconditional drain; T24 will add an explicit
    /// timeout that force-aborts dispatchers if the drain stalls.
    /// Idempotent: a second call after shutdown is a no-op.
    pub async fn shutdown(self: Arc<Self>) {
        self.shutdown.cancel();
        let accept_handle = self.accept_loop_handle.lock().take();
        if let Some(handle) = accept_handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
        // Listener is closed (accept_loop has returned), so no new requests
        // can arrive. Existing dispatchers continue and decrement
        // `active_delegations` via `ActiveCallGuard::drop` as they finish.
        while self.active_count() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let flusher_handle = self.flusher_handle.lock().take();
        if let Some(handle) = flusher_handle {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
        }
        // Reference held only to keep the deps Arc alive until shutdown
        // completes; explicit drop documents intent.
        drop(self.deps.clone());
    }
}

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
                return success_response(id, json!({ "ok": true }));
            }
            crate::handlers::report_progress(deps.funnel.as_ref(), &ctx, args).await
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
            return error_response(id, -32601, format!("Method not found: {other}"));
        }
    };

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
}
