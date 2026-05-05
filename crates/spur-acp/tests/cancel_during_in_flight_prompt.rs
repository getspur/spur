//! Regression test for the ACP-thread sequential-loop bug that broke ESC
//! cancellation in the TUI.
//!
//! ## Original bug
//!
//! `acp_thread_main` (`crates/spur-acp/src/connection/native.rs`) ran
//! commands sequentially:
//!
//! ```ignore
//! while let Some(cmd) = cmd_rx.recv().await {
//!     match cmd {
//!         AcpCommand::Prompt { request, reply } => {
//!             let prompt_result =
//!                 cx.send_request(request).block_task().await; // blocks the entire turn
//!             ...
//!         }
//!         AcpCommand::Cancel { session_id, reply } => {
//!             cx.send_notification(CancelNotification::new(session_id))
//!         }
//!         ...
//!     }
//! }
//! ```
//!
//! Because the loop awaited the in-flight prompt's response, an
//! `AcpCommand::Cancel` arriving on `cmd_rx` mid-stream queued behind it.
//! The orchestrator's `b.connection.cancel(...).await` blocked on its
//! oneshot reply, never returning, so the user's ESC had no effect on the
//! in-flight stream.
//!
//! ## Fix
//!
//! The Prompt and LoadSession arms now multiplex command intake against
//! the in-flight request future via biased `tokio::select!`, with
//! `Cancel` dispatched as a one-way `session/cancel` notification while
//! the prompt remains pending.
//!
//! ## Test design
//!
//! The bash fixture (`tests/fixtures/agent_held_prompt.sh`) holds its
//! `session/prompt` response until a release file appears on disk, and
//! records receipt of `session/cancel` by writing a flag file. The test
//! issues `prompt()`, then `cancel()` while the prompt is still pending,
//! and asserts that `cancel()` returns within 250 ms. The release file is
//! created from a background task after 500 ms so the prompt eventually
//! completes and the test can shut down cleanly.
//!
//! Timing margins (500 ms hold / 250 ms cancel-elapsed) were widened from
//! the original 150 ms / 50 ms after dual-gate review flagged 50 ms as
//! flake-prone on loaded CI runners. The 2× discriminator vs the bug's
//! ~500 ms+ cancel latency is preserved.

use std::time::{Duration, Instant};

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, PromptRequest, ProtocolVersion, TextContent,
};
use spur_acp::connection::{native::NativeAcpConnection, AgentConnection};

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_in_flight_prompt_returns_within_250ms() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script_path = format!("{manifest_dir}/tests/fixtures/agent_held_prompt.sh");
    assert!(
        std::path::Path::new(&script_path).exists(),
        "fixture missing at {script_path}"
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let release_path = temp.path().join("release_prompt");
    let cancel_seen_path = temp.path().join("cancel_seen");

    // Pass paths as positional args, NOT env vars — `std::env::set_var`
    // mutates process-global state and would race across parallel
    // `cargo test` workers.
    let extra_args = vec![
        script_path,
        release_path.to_string_lossy().into_owned(),
        cancel_seen_path.to_string_lossy().into_owned(),
    ];
    let mut conn = NativeAcpConnection::new("mock-held", "bash", extra_args, None);

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
        vec![ContentBlock::Text(TextContent::new(
            "hold-please".to_string(),
        ))],
    );

    // prompt() must return the empty stream immediately (within a generous
    // 100 ms slop) — the held-prompt response is on the wire, not at this
    // boundary.
    let _stream = tokio::time::timeout(Duration::from_millis(100), conn.prompt(prompt_req))
        .await
        .expect("prompt() must return its empty stream promptly")
        .expect("prompt() should not error");

    // Schedule the prompt release after 500 ms. Without this, the test would
    // hang the bash fixture.
    let release_path_for_task = release_path.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::fs::write(&release_path_for_task, b"go").expect("write release file");
    });

    // The actual regression assertion: cancel() must return within 250 ms,
    // even though the prompt response is held for 500 ms.
    let cancel_started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(2),
        conn.cancel(session.session_id.0.as_ref()),
    )
    .await
    .expect("cancel() should return well before the prompt response arrives")
    .expect("cancel() should not error against the mock");
    let cancel_elapsed = cancel_started.elapsed();

    assert!(
        cancel_elapsed <= Duration::from_millis(250),
        "cancel() took {cancel_elapsed:?}; the in-flight prompt was holding the loop"
    );

    // Side-channel proof that the cancel notification reached the agent.
    // Poll for up to 500 ms — bash IO has its own scheduling slop.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut saw_cancel = false;
    while tokio::time::Instant::now() < deadline {
        if std::fs::read_to_string(&cancel_seen_path)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            saw_cancel = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_cancel, "mock agent never observed session/cancel");

    // Let the release task fire so the prompt completes, then shut down.
    release_task.await.expect("release task should join");
    conn.shutdown().await.expect("shutdown should complete");
}
