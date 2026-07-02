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

// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

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
fn build_orchestrator_with_failing_connect() -> (
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
                input_rx, None, // brain_override: use config default
                None, // permission_tx
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
            let ev = match events_rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("event broadcast closed before BrainError was emitted");
                }
            };
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

// ── Task 3: load-failure path ─────────────────────────────────────────────────
//
// COVERAGE NOTE (DONE_WITH_CONCERNS):
//
// The fix at orchestrator.rs:1430-1433 changes `SessionId::new()` →
// `SessionId(original_session_id.clone())` in the `load_brain_session` Err arm.
//
// Triggering that code path in an integration test requires:
//   1. `connect_brain` to SUCCEED — which requires a real ACP subprocess to
//      initialize (the registry lookup passes, then `connection.initialize()`
//      calls the subprocess at orchestrator.rs:2247-2249).
//   2. `load_brain_session` to then FAIL — which requires the subprocess to
//      accept initialization but reject the session load.
//
// Without a mock ACP implementation (not present in this codebase), we cannot
// construct a `build_orchestrator_with_failing_load` helper that makes step 1
// succeed without a live agent process.
//
// What IS covered:
//   - The connect-failure path (orchestrator.rs:1357-1360) is fully tested by
//     `brain_error_on_resume_connect_failure_carries_requested_session_id` above.
//   - Both emit sites share the same fix shape; the connect-failure test guards
//     the broader invariant: BrainError.session must not be SessionId::new().
//   - The fix at line 1430 is structurally identical to the fix at line 1358
//     (already verified by Task 2's test), reducing the residual risk.
//
// If a MockAgentConnection trait implementation is added to the test harness in
// the future, a load-failure test can be added here following the same pattern
// as the connect-failure test above.
