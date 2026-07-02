//! Regression test for H5 — notifications arriving after the corresponding
//! `session/prompt` response frame must still reach the caller.
//!
//! ## Original bug
//!
//! The ACP thread used to swap `notification_tx` for a throwaway `dead_tx` as
//! soon as `connection.prompt().await` returned. If the ACP SDK scheduled any
//! `session_notification` callback on its LocalSet *after* that swap, the
//! notification was sent to the dead channel and silently dropped.
//!
//! ## Current architecture
//!
//! Notifications are now published into a connection-scoped
//! `broadcast::Sender<SessionNotification>` owned by `NativeAcpConnection`.
//! The per-turn `Stream` returned by `prompt()` is always empty for this
//! transport; callers that need notifications subscribe via
//! `conn.subscribe_session_notifications()` **before** calling `prompt()`.
//!
//! ## Test design
//!
//! Same fixture as before (`tests/fixtures/agent_trailing_notification.sh`):
//!   1. emits a `session/update` (chunk: "first") BEFORE the prompt response,
//!   2. emits the `session/prompt` response (stopReason: end_turn),
//!   3. sleeps 200 ms,
//!   4. emits a trailing `session/update` (chunk: "second").
//!
//! Expected behavior: the caller observes BOTH chunks via the broadcast receiver.

use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, PromptRequest, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use spur_acp::connection::{native::NativeAcpConnection, AgentConnection};

#[tokio::test(flavor = "multi_thread")]
async fn trailing_notification_reaches_caller() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script_path = format!("{manifest_dir}/tests/fixtures/agent_trailing_notification.sh");
    assert!(
        std::path::Path::new(&script_path).exists(),
        "fixture missing at {script_path}"
    );

    let mut conn =
        NativeAcpConnection::new("mock-trailing", "bash", vec![script_path.clone()], None);

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("initialize should succeed against mock");

    let cwd = std::env::current_dir().expect("cwd");
    let session = conn
        .new_session(cwd, vec![])
        .await
        .expect("new_session should succeed against mock");

    // Subscribe BEFORE prompt() so we don't miss any notifications.
    let mut rx = conn
        .subscribe_session_notifications()
        .expect("NativeAcpConnection must provide a session-notification subscriber");

    let prompt_req = PromptRequest::new(
        session.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(
            "any-prompt".to_string(),
        ))],
    );

    // Fire prompt() — the returned stream is always empty for this transport;
    // notifications arrive on the broadcast receiver `rx` instead.
    let _stream = conn.prompt(prompt_req).await.expect("prompt");

    // Drain from the broadcast receiver for up to 1 s — long enough for the
    // 200 ms sleep in the fixture plus handler scheduling slop.
    let mut chunks: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(notif)) => chunks.push(format!("{notif:?}")),
            Ok(Err(_)) => break, // broadcast sender dropped (connection torn down)
            Err(_) => break,     // deadline
        }
    }

    // Best-effort teardown so the mock exits cleanly.
    let _ = conn.shutdown().await;

    assert!(
        chunks.iter().any(|s| s.contains("first")),
        "expected leading chunk \"first\" in broadcast, got: {chunks:?} \
         — fixture/API wiring is wrong (this must pass regardless of H5)"
    );
    assert!(
        chunks.iter().any(|s| s.contains("second")),
        "expected trailing chunk \"second\" in broadcast, got: {chunks:?} \
         — H5 regressed: trailing notifications are being dropped. \
         Ensure the broadcast sender in SpurAcpClientDynamic is alive \
         for the full connection lifetime."
    );
}
