//! Per-`BrainSession` HTTP/JSON-RPC server exposing the curated worker MCP
//! tool subset to delegated workers.
//!
//! T15 implements the lifecycle skeleton: bind a TCP listener on
//! `127.0.0.1:0`, run a cancellable accept loop, and expose `start()` /
//! `url()` / `shutdown()`. T16 adds HMAC token validation middleware.
//! JSON-RPC dispatch (T17), audit (T18+) and the rest of Phase 4 add
//! behaviour on top.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rand::RngCore;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::McpEventSink;
use crate::handlers::WorkerCallContext;
use crate::token::{validate_token, TokenError};

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
    shutdown: CancellationToken,
    accept_loop_handle: Mutex<Option<JoinHandle<()>>>,
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
        _pm_service: Arc<spur_pm::PmService>,
        _feature_gate: Arc<spur_license::FeatureGate>,
        _funnel: Arc<dyn McpEventSink>,
    ) -> Result<Arc<Self>, BindError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let mut hmac_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut hmac_key);

        let shutdown = CancellationToken::new();
        let server = Arc::new(Self {
            addr,
            hmac_key,
            brain_session_id: brain_session_id.clone(),
            shutdown: shutdown.clone(),
            accept_loop_handle: Mutex::new(None),
        });

        let handle =
            tokio::spawn(accept_loop(listener, shutdown, hmac_key, brain_session_id));
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
        self.issue_token_with_expiry(delegation_id, now + ttl.as_secs())
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

    /// Cancel the accept loop and wait up to 5 seconds for it to exit.
    /// Idempotent: a second call after shutdown is a no-op.
    pub async fn shutdown(self: Arc<Self>) {
        self.shutdown.cancel();
        let handle = self.accept_loop_handle.lock().take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
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
) {
    // TODO(T17): track per-connection tasks via tokio_util::task::TaskTracker
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
/// 401 or dispatch to the JSON-RPC handler (T17).
async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    hmac_key: [u8; 32],
    brain_session_id: String,
) {
    let response = match handle_request(&mut stream, &hmac_key, &brain_session_id).await {
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

    dispatch(ctx, body).await
}

/// Stub dispatcher. T17 replaces this with the real JSON-RPC router.
async fn dispatch(_ctx: WorkerCallContext, body: Vec<u8>) -> Result<String, TokenError> {
    let id = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);

    Ok(serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": -32601,
            "message": "Method not found"
        },
        "id": id
    })
    .to_string())
}
