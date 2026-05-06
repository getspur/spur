//! Shared helpers for spur-mcp integration tests.
//!
//! Currently only hosts the loopback-bind probe used by tests that exercise
//! `McpCallbackServer::start()`. Some sandboxes (seccomp profiles, restricted
//! container runtimes) deny loopback `bind(2)` with EPERM. Tests that touch
//! the listener skip gracefully in that environment rather than hard-failing.

#![allow(dead_code)]

use tokio::net::TcpListener;
use tokio::sync::OnceCell;

pub mod beads;
pub mod g_strict_harness;
pub mod server_builder;

static LOOPBACK_BINDABLE: OnceCell<bool> = OnceCell::const_new();

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
