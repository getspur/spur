//! Per-`BrainSession` HTTP/JSON-RPC server exposing the curated worker MCP
//! tool subset to delegated workers.
//!
//! T15 (this file) implements only the lifecycle skeleton: bind a TCP listener
//! on `127.0.0.1:0`, run a cancellable accept loop, and expose `start()` /
//! `url()` / `shutdown()`. Token middleware (T16), JSON-RPC dispatch (T17),
//! audit (T18+) and the rest of Phase 4 add behavior on top.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rand::RngCore;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::McpEventSink;

/// Per-`BrainSession` worker MCP server. Owns a TCP listener on
/// `127.0.0.1:<random-port>` and a cancellable accept loop.
pub struct WorkerMcpServer {
    addr: SocketAddr,
    /// 32-byte HMAC key generated at start. Held in-process only — never
    /// logged or persisted. Used by token validation middleware (T16+).
    #[allow(dead_code)]
    hmac_key: [u8; 32],
    /// `BrainSession` id stamped onto every `WorkerCallContext` issued by this
    /// server (T16+).
    #[allow(dead_code)]
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
            brain_session_id,
            shutdown: shutdown.clone(),
            accept_loop_handle: Mutex::new(None),
        });

        let handle = tokio::spawn(accept_loop(listener, shutdown));
        *server.accept_loop_handle.lock() = Some(handle);

        Ok(server)
    }

    /// The canonical `http://127.0.0.1:<port>/mcp` URL workers POST JSON-RPC
    /// requests to.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
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
async fn accept_loop(listener: TcpListener, shutdown: CancellationToken) {
    // TODO(T17): track per-connection tasks via tokio_util::task::TaskTracker
    // so shutdown drains in-flight requests instead of detaching them.
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            accept = listener.accept() => match accept {
                Ok((stream, _peer)) => {
                    tokio::spawn(handle_connection(stream));
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

/// Stub for T15. T16 replaces this with token-validating middleware; T17 with
/// the full JSON-RPC dispatcher. Writes a minimal HTTP 401 so reachability
/// checks succeed before the request engine is in place.
async fn handle_connection(mut stream: TcpStream) {
    let _ = stream
        .write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
    let _ = stream.shutdown().await;
}
