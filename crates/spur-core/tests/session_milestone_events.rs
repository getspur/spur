//! Integration test — Tranche 2 Task 2: resume-pipeline milestone events.
//!
//! Verifies that `SessionRetireStart` and `BrainConnecting` are emitted on
//! the `ResumeSession` path at the correct phase boundaries.
//!
//! ## Coverage scope
//!
//! | Milestone          | Tested? | Notes                                      |
//! |--------------------|---------|--------------------------------------------|
//! | SessionRetireStart | YES     | Fires before connect_brain (no brain held) |
//! | SessionRetireComplete | PARTIAL | Fires only when there is an active brain to retire; not tested here because the harness never establishes a prior brain session |
//! | BrainConnecting    | YES     | Fires before connect_brain is attempted    |
//! | SessionLoading     | NO      | Requires connect_brain to succeed (needs live ACP subprocess) |
//! | SessionLoaded      | NO      | Requires load_brain_session to succeed (needs live ACP subprocess) |
//!
//! ## Gap documentation (DONE_WITH_CONCERNS)
//!
//! `SessionLoading` and `SessionLoaded` fire only after `connect_brain`
//! succeeds.  That requires a real ACP subprocess to initialize (the registry
//! lookup passes, then `connection.initialize()` calls the subprocess).
//! Without a mock ACP implementation, we cannot drive the orchestrator past the
//! connect phase without a live agent process.  This gap is identical to the one
//! documented in `brain_error_session_correlation.rs` (Task 3 coverage note).
//!
//! If a `MockAgentConnection` is added to the test harness in the future,
//! `SessionLoading` / `SessionLoaded` tests can be appended here following
//! the same harness pattern.

use std::time::Duration;

use spur_acp::config::SpurConfig;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::new_overflow_buf;
use spur_core::orchestrator::InteractiveInput;
use spur_core::Orchestrator;
use tokio::sync::mpsc;

// ── Harness ───────────────────────────────────────────────────────────────────

/// Build an orchestrator with an empty agent registry so `connect_brain` fails
/// immediately with "Brain agent 'claude-code' not found".
///
/// Returns `(input_tx, events_rx)`.  The background `run_interactive` task
/// exits when `input_tx` is dropped.
///
/// NOTE: Per Tranche 1 policy, this harness is intentionally duplicated from
/// `brain_error_session_correlation.rs` rather than shared — no refactoring.
fn build_orchestrator_with_failing_connect() -> (
    mpsc::Sender<InteractiveInput>,
    tokio::sync::broadcast::Receiver<SpurEvent>,
) {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    // SpurConfig::default() → brain.default = "claude-code".
    // Empty registry → connect_brain fails the registry lookup without
    // touching any subprocess.
    let orch = Orchestrator::new(tmp.path().into(), SpurConfig::default(), None)
        .expect("Orchestrator::new");

    // Subscribe BEFORE spawning so we never miss early events.
    let events_rx = orch.event_tx.subscribe();

    let (input_tx, input_rx) = mpsc::channel::<InteractiveInput>(16);
    let overflow = new_overflow_buf();

    tokio::spawn(async move {
        let _ = orch
            .run_interactive(
                input_rx,
                None, // brain_override: use config default
                None, // permission_tx
                overflow,
            )
            .await;
        // Keep tmp alive until run_interactive exits.
        drop(tmp);
    });

    (input_tx, events_rx)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `SessionRetireStart.to` must equal the session id passed in `ResumeSession`.
///
/// Because the orchestrator starts with no active brain, `from` will be `None`
/// and `to` will be the target session.  The event must fire BEFORE
/// `connect_brain` is attempted (and fails).
#[tokio::test]
async fn resume_emits_session_retire_start_with_faithful_target() {
    let target_str = "milestone-target";
    let target = SessionId(target_str.to_string());

    let (input_tx, mut events_rx) = build_orchestrator_with_failing_connect();

    input_tx
        .send(InteractiveInput::ResumeSession {
            session_id: target_str.to_string(),
        })
        .await
        .expect("send ResumeSession");

    let mut saw_retire_start = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

    while tokio::time::Instant::now() < deadline {
        let ev = match tokio::time::timeout(Duration::from_millis(500), events_rx.recv()).await {
            Ok(Ok(ev)) => ev,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => continue,
        };

        if let SpurEventBody::SessionRetireStart { to, from } = &ev.body {
            assert_eq!(
                *to, target,
                "SessionRetireStart.to must match the resume target"
            );
            assert!(
                from.is_none(),
                "SessionRetireStart.from must be None when no prior brain is active; got {:?}",
                from
            );
            saw_retire_start = true;
            break;
        }

        // connect_brain fails → BrainError is emitted.  If we reach this
        // without having seen SessionRetireStart, the assertion below will
        // catch it.
        if matches!(ev.body, SpurEventBody::BrainError { .. }) {
            break;
        }
    }

    assert!(
        saw_retire_start,
        "SessionRetireStart was never emitted on the resume path"
    );
}

/// `BrainConnecting` must fire before `connect_brain` is attempted, carrying
/// the resume target session id.
#[tokio::test]
async fn resume_emits_brain_connecting_before_connect_fails() {
    let target_str = "milestone-brain-connecting";
    let target = SessionId(target_str.to_string());

    let (input_tx, mut events_rx) = build_orchestrator_with_failing_connect();

    input_tx
        .send(InteractiveInput::ResumeSession {
            session_id: target_str.to_string(),
        })
        .await
        .expect("send ResumeSession");

    let mut saw_brain_connecting = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

    while tokio::time::Instant::now() < deadline {
        let ev = match tokio::time::timeout(Duration::from_millis(500), events_rx.recv()).await {
            Ok(Ok(ev)) => ev,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => continue,
        };

        if let SpurEventBody::BrainConnecting { session, .. } = &ev.body {
            assert_eq!(
                *session, target,
                "BrainConnecting.session must match the resume target"
            );
            saw_brain_connecting = true;
            break;
        }

        if matches!(ev.body, SpurEventBody::BrainError { .. }) {
            break;
        }
    }

    assert!(
        saw_brain_connecting,
        "BrainConnecting was never emitted on the resume path (before connect_brain failed)"
    );
}
