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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let mut hmac_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut hmac_key);

        let materializer = OutcomeMaterializer::new(Arc::clone(&deps.outcome_store));
        let delegations = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
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
        });

        let shutdown = CancellationToken::new();
        let server = Arc::new(Self {
            addr,
            hmac_key,
            brain_session_id: brain_session_id.clone(),
            deps: Arc::clone(&dispatcher_deps),
            shutdown: shutdown.clone(),
            accept_loop_handle: Mutex::new(None),
        });

        let handle = tokio::spawn(accept_loop(
            listener,
            shutdown,
            hmac_key,
            brain_session_id,
            dispatcher_deps,
        ));
        *server.accept_loop_handle.lock() = Some(handle);

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

    /// Cancel the accept loop and wait up to 5 seconds for it to exit.
    /// Idempotent: a second call after shutdown is a no-op.
    pub async fn shutdown(self: Arc<Self>) {
        self.shutdown.cancel();
        let handle = self.accept_loop_handle.lock().take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
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
            let body =
                r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request"},"id":null}"#;
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
    path_and_query
        .split_once('?')
        .and_then(|(_, query)| {
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
    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));

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
        "get_issue" => crate::handlers::get_issue(deps.pm_service.as_ref(), &ctx, args).await,
        "list_issues" => crate::handlers::list_issues(deps.pm_service.as_ref(), &ctx, args).await,
        "update_issue" => {
            crate::handlers::update_issue(deps.pm_service.as_ref(), &ctx, args).await
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
            crate::handlers::get_plan_status(
                deps.plan_resolver.as_ref(),
                &deps.reconciler_outcomes,
                &ctx,
                args,
            )
            .await
        }
        "fetch_outcome_artifact" => {
            crate::handlers::fetch_outcome_artifact(
                &deps.materializer,
                deps.outcome_store.as_ref(),
                &ctx,
                args,
            )
            .await
        }
        "get_task_diff" => {
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
