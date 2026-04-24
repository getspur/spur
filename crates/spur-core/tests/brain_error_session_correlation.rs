//! Regression test — FP-5: BrainError.session must equal the session the user
//! asked to resume, not a freshly generated SessionId::new().
//!
//! ## Bug (guard against regression)
//!
//! `orchestrator.rs` lines 1357-1360 and 1430-1433 emit:
//!
//! ```ignore
//! self.emit(SpurEvent::now(SpurEventBody::BrainError {
//!     session: SessionId::new(),   // BUG: random UUID
//!     message: error_message,
//! }));
//! ```
//!
//! When a user picks a session to resume and `connect_brain` (or
//! `load_brain_session`) fails, consumers that correlate events on
//! `session` never match because the emitted id is a fresh UUID,
//! not the target session id the user requested.
//!
//! ## What this test pins
//!
//! Drive the orchestrator through a `ResumeSession` where `connect_brain`
//! fails (empty registry → "Brain agent 'claude-code' not found"), capture
//! the `BrainError` event, and assert `session == SessionId(TARGET)`.
//!
//! Expected state:
//! - Current HEAD (pre-fix): FAILS — `session` is a random UUID.
//! - After Task 2 fix at orchestrator.rs:1358: PASSES.

use std::time::Duration;

use spur_acp::config::SpurConfig;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::new_overflow_buf;
use spur_core::orchestrator::InteractiveInput;
use spur_core::Orchestrator;
use tokio::sync::mpsc;

/// Build an orchestrator with an empty agent registry (so `connect_brain`
/// will fail immediately with "Brain agent 'claude-code' not found").
///
/// Returns:
/// - The broadcast receiver subscribed BEFORE `run_interactive` spawns
///   (avoids a race where early events are missed).
/// - The ingress `mpsc::Sender<InteractiveInput>` for driving inputs.
///
/// `run_interactive` is spawned as a background task; it will terminate
/// when the ingress sender is dropped.
fn build_orchestrator_with_failing_connect()
-> (
    mpsc::Sender<InteractiveInput>,
    tokio::sync::broadcast::Receiver<spur_acp::domain::events::SpurEvent>,
) {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    // SpurConfig::default() has brain.default = "claude-code".
    // An empty registry means connect_brain will fail the registry lookup
    // at orchestrator.rs:2237-2241 before touching any subprocess.
    let orch = Orchestrator::new(tmp.path().into(), SpurConfig::default(), None)
        .expect("Orchestrator::new");

    // Subscribe BEFORE spawning the loop so we don't miss the BrainError.
    let events_rx = orch.event_tx.subscribe();

    let (input_tx, input_rx) = mpsc::channel::<InteractiveInput>(16);
    let overflow = new_overflow_buf();

    // Spawn run_interactive. We don't join this task in the test; it will
    // exit when input_tx is dropped at end of the test function.
    tokio::spawn(async move {
        let _ = orch
            .run_interactive(
                input_rx,
                None,          // brain_override: use config default
                None,          // permission_tx
                overflow,
            )
            .await;
        // Keep tmp alive until here so the repo_root exists while the loop runs.
        drop(tmp);
    });

    (input_tx, events_rx)
}

/// Guard FP-5: `BrainError.session` on resume-connect failure must carry the
/// session id the user requested, not a fresh UUID.
///
/// Pre-fix: FAILS with assertion diff showing a random UUID on the left.
/// Post-fix (Task 2, orchestrator.rs:1358): PASSES.
#[tokio::test]
async fn brain_error_on_resume_connect_failure_carries_requested_session_id() {
    // 1. Known session id the user asked to resume.
    let target = "target-session-under-test";

    // 2. Build orchestrator wired to fail on connect_brain (empty registry).
    let (input_tx, mut events_rx) = build_orchestrator_with_failing_connect();

    // 3. Send ResumeSession with the known target id.
    input_tx
        .send(InteractiveInput::ResumeSession {
            session_id: target.to_string(),
        })
        .await
        .expect("send ResumeSession");

    // 4. Collect events until we see BrainError or time out (2 s is generous;
    //    the registry lookup is synchronous and fails instantly).
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let ev = events_rx.recv().await.expect("broadcast closed");
            if let SpurEventBody::BrainError { session, message } = &ev.body {
                return (session.clone(), message.clone());
            }
        }
    })
    .await
    .expect("BrainError event never emitted within 2 s");

    // 5. The fix lives here: session must equal what the user requested.
    assert_eq!(
        result.0,
        SessionId(target.to_string()),
        "BrainError.session must carry the resume target SessionId(\"{target}\"), \
         not a fresh SessionId::new() UUID. \
         Got: {:?}. \
         This test is expected to FAIL on current HEAD (pre-fix). \
         Task 2 fixes orchestrator.rs:1358.",
        result.0,
    );
}
