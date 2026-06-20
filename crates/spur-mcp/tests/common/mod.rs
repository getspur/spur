//! Shared helpers for spur-mcp integration tests.
//!
//! Currently only hosts the loopback-bind probe used by tests that exercise
//! `McpCallbackServer::start()`. Some sandboxes (seccomp profiles, restricted
//! container runtimes) deny loopback `bind(2)` with EPERM. Tests that touch
//! the listener skip gracefully in that environment rather than hard-failing.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use spur_acp::SpurEventBody;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::{McpHandlerError, WorkerCallContext};
use spur_mcp::worker_server::WorkerSignalSink;
use tokio::net::TcpListener;
use tokio::sync::OnceCell;

pub mod beads;
pub mod g_strict_harness;
pub mod server_builder;

static LOOPBACK_BINDABLE: OnceCell<bool> = OnceCell::const_new();

pub struct TestWorkerSignalSink {
    funnel: Arc<dyn McpEventSink>,
}

impl TestWorkerSignalSink {
    pub fn new(funnel: Arc<dyn McpEventSink>) -> Self {
        Self { funnel }
    }
}

#[async_trait]
impl WorkerSignalSink for TestWorkerSignalSink {
    async fn report_signal(
        &self,
        _ctx: &WorkerCallContext,
        _args: Value,
    ) -> Result<Value, McpHandlerError> {
        Err(McpHandlerError::Unauthorized(
            "test signal sink is not licensed".into(),
        ))
    }

    async fn report_progress(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        #[derive(serde::Deserialize)]
        struct Args {
            message: String,
            #[serde(default)]
            percent: Option<f64>,
        }

        let Args { message, percent } = serde_json::from_value(args)
            .map_err(|e| McpHandlerError::InvalidParams(format!("invalid args: {e}")))?;
        let _ = self.funnel.try_emit(SpurEventBody::WorkerReportProgress {
            delegation_id: ctx.delegation_id.clone(),
            message,
            percent,
        });

        Ok(json!({ "ok": true }))
    }
}

/// Probe `127.0.0.1:0` at most once per test binary. The result is cached
/// after up to 3 bind attempts so that a transient port-exhaustion blip on a
/// healthy host does not permanently latch the binary into skip mode. EPERM
/// in a sandbox is immediate, so the retry budget costs only a few
/// microseconds in the failure path.
pub async fn loopback_bindable() -> bool {
    *LOOPBACK_BINDABLE
        .get_or_init(|| async {
            for _ in 0..3 {
                if TcpListener::bind("127.0.0.1:0").await.is_ok() {
                    return true;
                }
            }
            false
        })
        .await
}

/// Skip the current test (printing a sandbox note) when loopback bind is denied.
///
/// Use the unary form for `async fn name()` and the binary form for tests with
/// non-`()` return shapes (e.g. `Result<(), Box<dyn Error>>`), passing the
/// expression to early-return: `skip_if_no_loopback!("name", Ok(()));`.
#[macro_export]
macro_rules! skip_if_no_loopback {
    ($name:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return;
        }
    };
    ($name:expr, $ret:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return $ret;
        }
    };
}
