//! Regression test for H5 — the `dead_tx` race in `NativeAcpConnection`
//! that drops trailing `session/update` notifications arriving after the
//! corresponding `session/prompt` response frame has been consumed.
//!
//! ## Bug reproduction
//!
//! The ACP thread swaps `notification_tx` for a throwaway `dead_tx` as
//! soon as `connection.prompt().await` returns. If the ACP SDK schedules
//! any `session_notification` callback on its LocalSet *after* that swap,
//! the notification is sent to the dead channel and silently dropped.
//!
//! User-visible symptom: the tail of a worker's output is truncated
//! ("the message breaks at the end").
//!
//! ## Test design
//!
//! We drive the real `NativeAcpConnection` against a deterministic mock
//! agent (`tests/fixtures/agent_trailing_notification.sh`) that:
//!   1. emits a `session/update` (chunk: "first") BEFORE the prompt response,
//!   2. emits the `session/prompt` response (stopReason: end_turn),
//!   3. sleeps 200 ms,
//!   4. emits a trailing `session/update` (chunk: "second").
//!
//! Expected (correct) behavior: the caller's stream yields BOTH chunks.
//! Current (buggy) behavior: the "second" chunk is dropped because it
//! arrives after the dead_tx swap.
//!
//! Task 2 of the SpurEvent Stream Backbone plan implements a 250 ms grace
//! window after `prompt()` returns so stragglers like this are still
//! forwarded — at which point this test will pass.

use std::time::Duration;

use agent_client_protocol::{
    ContentBlock, InitializeRequest, PromptRequest, ProtocolVersion, TextContent,
};
use futures::StreamExt;
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

    let prompt_req = PromptRequest::new(
        session.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new("any-prompt".to_string()))],
    );

    let mut stream = conn.prompt(prompt_req).await.expect("prompt");

    // Drain for up to 1 s — long enough for the 200 ms sleep in the fixture
    // plus handler scheduling slop. A correct implementation emits "second"
    // well within this window; the buggy dead_tx swap silently drops it,
    // and our bounded drain will fall off the deadline.
    let mut chunks: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(notif)) => chunks.push(format!("{notif:?}")),
            Ok(None) => break, // stream closed (sender dropped)
            Err(_) => break,   // deadline
        }
    }

    // Best-effort teardown so the mock exits cleanly.
    let _ = conn.shutdown().await;

    assert!(
        chunks.iter().any(|s| s.contains("first")),
        "expected leading chunk \"first\" in stream, got: {chunks:?} \
         — fixture/API wiring is wrong (this must pass regardless of H5)"
    );
    assert!(
        chunks.iter().any(|s| s.contains("second")),
        "expected trailing chunk \"second\" in stream, got: {chunks:?} \
         — H5 regressed: notifications arriving after prompt() returns are \
         being routed to dead_tx and dropped. Implement a grace window on \
         notification_tx swap (see SpurEvent Stream Backbone plan, Task 2)."
    );
}
