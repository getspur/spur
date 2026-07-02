//! Regression test for FP-3: "every state has a bounded exit".
//!
//! Exercises `shutdown_mcp_server` via the `test_support` shim.
//! The fake MCP server's `shutdown()` resolves immediately; the guard wraps a
//! task that never completes. Before the Task-5 fix the function hangs
//! indefinitely at `guard.await`. After the fix it returns within
//! `MCP_SHUTDOWN_TIMEOUT` + epsilon.

// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

use std::sync::Arc;
use std::time::{Duration, Instant};

use spur_acp::types::SessionId;
use spur_core::event_funnel::test_channel;
use spur_core::test_support::{
    call_shutdown_mcp_server, RetirableMcpServer, MCP_SHUTDOWN_TIMEOUT_MS,
};

/// Fake MCP server whose `shutdown()` resolves instantly.
/// Isolates the bug to the `guard.await` line, not `server.shutdown()`.
struct InstantShutdownServer;

impl RetirableMcpServer for InstantShutdownServer {
    fn mark_retiring(&self) {}
    fn cancel_in_flight_workers(&self) {}
    fn force_abort(&self) {}
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn shutdown_mcp_server_returns_even_if_guard_task_hangs() {
    // Guard wraps a task that never finishes — simulates a stuck background
    // task (e.g. an MCP stdio process that never exits).
    let stuck_task = tokio::spawn(async {
        // Block forever without consuming CPU.
        std::future::pending::<()>().await;
    });
    let guard = tokio_util::task::AbortOnDropHandle::new(stuck_task);

    let server: Arc<dyn RetirableMcpServer> = Arc::new(InstantShutdownServer);

    // Construct a minimal FunnelHandle that accepts emitted events.
    let (funnel, _rx) = test_channel();

    let session = SessionId("under-test".to_string());

    let started = Instant::now();
    call_shutdown_mcp_server(&funnel, &session, Some(server), Some(guard)).await;
    let elapsed = started.elapsed();

    // Must return within MCP_SHUTDOWN_TIMEOUT (5 s) + 500 ms epsilon.
    // Before the fix this hangs indefinitely; the tokio test harness will
    // eventually panic after its own timeout, but the assertion below also
    // catches the case where somehow we return late.
    let ceiling = Duration::from_millis(MCP_SHUTDOWN_TIMEOUT_MS + 500);
    assert!(
        elapsed < ceiling,
        "shutdown_mcp_server hung on stuck guard: took {elapsed:?} (ceiling {ceiling:?})"
    );
}

#[tokio::test]
async fn shutdown_mcp_server_bounds_guard_on_none_server_early_return() {
    // Covers the early-return branch at orchestrator.rs:222:
    //   mcp_server.take() returns None, but mcp_guard is Some(stuck).
    // Pre-fix: guard.await at line 222 hangs forever.
    // Post-fix: tokio::time::timeout bounds it at MCP_SHUTDOWN_TIMEOUT.

    let stuck_task = tokio::spawn(async {
        std::future::pending::<()>().await;
    });
    let guard = tokio_util::task::AbortOnDropHandle::new(stuck_task);

    // Key difference from the first test: pass None for the server.
    let server: Option<Arc<dyn RetirableMcpServer>> = None;

    let (funnel, _rx) = test_channel();
    let session = SessionId("under-test-none-server".to_string());

    let started = Instant::now();
    call_shutdown_mcp_server(&funnel, &session, server, Some(guard)).await;
    let elapsed = started.elapsed();

    let ceiling = Duration::from_millis(MCP_SHUTDOWN_TIMEOUT_MS + 500);
    assert!(
        elapsed < ceiling,
        "shutdown_mcp_server early-return hung on stuck guard: took {elapsed:?} (ceiling {ceiling:?})"
    );
}
