//! Regression test for the claude-code-acp dropped-notification race.
//!
//! ## Bug description
//!
//! `claude-code-acp` advertises ~144 slash commands via ACP
//! `session/update{available_commands_update}`, but the commands never appear
//! in the spur command popup. Logs confirm the notifications arrive on the
//! wire but are dropped by `NativeAcpConnection`'s per-turn channel logic:
//! a LocalSet-scheduled `session_notification` callback fires AFTER the
//! per-turn `notification_tx` has been swapped to `dead_tx`.
//!
//! The existing grace-window mitigation (native.rs:1026-1038) also fails
//! because it tracks the callback's own timestamp, not the SDK's wire-parse
//! time. Notifications emitted outside of a `prompt()` call — such as those
//! generated right after `session/load` — have no per-turn channel at all and
//! are silently dropped unconditionally.
//!
//! ## Test design
//!
//! We drive the real `NativeAcpConnection` against a deterministic mock agent
//! (`tests/fixtures/agent_delayed_available_commands.sh`) that:
//!   1. returns `loadSession: true` in `initialize`,
//!   2. returns a normal `session/load` response,
//!   3. sleeps 50 ms, then emits `session/update{available_commands_update}`.
//!
//! The test calls `conn.subscribe_session_notifications()` to obtain a
//! `broadcast::Receiver<SessionNotification>` that is connected to a
//! connection-scoped bus, then waits up to 2 s for the delayed notification.
//!
//! ## Red state (Task 1)
//!
//! `AgentConnection::subscribe_session_notifications()` does not yet exist.
//! This test is intentionally written to compile-fail on that missing method,
//! establishing the TDD red state. Task 2+ will add the broadcast bus and
//! the trait method to make it pass.

use std::time::Duration;

use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion, SessionUpdate};
use futures::StreamExt;
use spur_acp::{
    connection::native::NativeAcpConnection, connection::AgentConnection, LoadSessionRequest,
};

#[tokio::test(flavor = "multi_thread")]
async fn delayed_available_commands_update_reaches_subscriber() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script_path = format!("{manifest_dir}/tests/fixtures/agent_delayed_available_commands.sh");
    assert!(
        std::path::Path::new(&script_path).exists(),
        "fixture missing at {script_path}"
    );

    let mut conn =
        NativeAcpConnection::new("mock-delayed", "bash", vec![script_path.clone()], None);

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("initialize should succeed against mock");

    // ── THIS LINE IS THE INTENTIONAL COMPILE FAILURE ──────────────────────
    //
    // `subscribe_session_notifications()` does not exist on `AgentConnection`
    // or `NativeAcpConnection` yet. Task 2+ will add:
    //
    //   fn subscribe_session_notifications(
    //       &self,
    //   ) -> Option<tokio::sync::broadcast::Receiver<SessionNotification>> {
    //       None  // default impl
    //   }
    //
    // and `NativeAcpConnection` will override it to return `Some(rx)` from a
    // connection-scoped `broadcast::Sender<SessionNotification>`.
    //
    // Until that method is added this test will not compile, and that is the
    // desired red state for this TDD task.
    let mut rx = conn
        .subscribe_session_notifications()
        .expect("NativeAcpConnection must provide a session-notification subscriber");

    // Trigger `session/load`. The fixture will respond and then, after 50 ms,
    // emit the delayed `available_commands_update` notification.
    let cwd = std::env::current_dir().expect("cwd");
    let (_load_response, _load_stream) = conn
        .load_session(LoadSessionRequest::new("test-session".to_string(), cwd))
        .await
        .expect("load_session should succeed against mock");

    // Drain the load stream (may be empty) so the connection is ready.
    let mut load_stream = _load_stream;
    let load_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = load_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, load_stream.next()).await {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }

    // Wait up to 2 s for the delayed available_commands_update to arrive on
    // the broadcast receiver. In the unfixed code the notification is dropped
    // and this will time out; in the fixed code it will arrive within ~50 ms.
    let notif = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for available_commands_update — notification was dropped")
        .expect("broadcast sender was dropped before notification arrived");

    match &notif.update {
        SessionUpdate::AvailableCommandsUpdate(update) => {
            assert_eq!(
                update.available_commands.len(),
                1,
                "expected 1 available command, got {}: {notif:?}",
                update.available_commands.len()
            );
            assert_eq!(
                update.available_commands[0].name, "test-cmd",
                "expected command name \"test-cmd\", got \"{}\": {notif:?}",
                update.available_commands[0].name
            );
        }
        other => panic!(
            "expected SessionUpdate::AvailableCommandsUpdate, got {other:?} — \
             the broadcast bus delivered the wrong notification variant"
        ),
    }

    // Best-effort teardown so the mock exits cleanly.
    let _ = conn.shutdown().await;
}
