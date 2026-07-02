//! End-to-end integration tests for the orchestrator review loopback.
//!
//! These exercise `run_gate_with_retries` — the test helper that mirrors
//! the production retry logic in `execute_delegation`. Real ACP agents
//! are not spawned here (that requires a live `spur watch` session — see
//! the manual smoke checklist in the spec).
//!
//! This file is the CI regression guard for the complete review-loopback
//! feature. It groups the six high-level outcome paths in one place:
//! Approve → Success, Reject → Rejected, Modify → Modified,
//! Retry×N then Approve → Success, Retry limit exceeded → Failed,
//! Timeout → TimedOut.
//!
//! Fine-grained gate tests (register/remove invariants, dispatcher routing,
//! cancellation audit events, etc.) live in `review_gate_integration.rs`.

// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
use spur_core::{test_support::run_gate_with_retries, ExecutorId, ReviewSink};
use std::time::Duration;

/// Shared driver: spawns a background task that delivers `decisions` to the
/// sink in order, then runs `run_gate_with_retries` and returns its result.
///
/// The driver tracks `attempt_n` locally and increments it after every
/// `Retry` decision, matching the counter that `run_gate_with_retries`
/// uses when it registers the next gate.
async fn drive(decisions: Vec<ReviewDecision>, max_retries: u32) -> DelegationStatus {
    let sink = ReviewSink::new();
    let sink_for_driver = sink.clone();
    let decisions_task = tokio::spawn(async move {
        let mut attempt = 1u32;
        for d in decisions {
            // Spin-yield until the gate has registered for this attempt_n.
            loop {
                tokio::task::yield_now().await;
                if sink_for_driver
                    .submit(ExecutorId::new("e1"), attempt, d.clone())
                    .await
                {
                    break;
                }
            }
            if matches!(d, ReviewDecision::Retry { .. }) {
                attempt += 1;
            }
        }
    });
    let status = run_gate_with_retries(
        ExecutorId::new("e1"),
        DelegationStatus::Success,
        Duration::from_secs(60),
        TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
        max_retries,
        sink,
    )
    .await;
    decisions_task.await.unwrap();
    status
}

// ─── Happy path: terminal decisions ───────────────────────────────────────

#[tokio::test]
async fn e2e_approve() {
    let s = drive(vec![ReviewDecision::Approve], 3).await;
    assert!(matches!(s, DelegationStatus::Success));
}

#[tokio::test]
async fn e2e_reject() {
    let s = drive(
        vec![ReviewDecision::Reject {
            reason: "nope".into(),
        }],
        3,
    )
    .await;
    match s {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "nope"),
        other => panic!("expected Rejected, got {:?}", other),
    }
}

#[tokio::test]
async fn e2e_modify() {
    let s = drive(
        vec![ReviewDecision::Modify {
            note: "fix naming".into(),
        }],
        3,
    )
    .await;
    match s {
        DelegationStatus::Modified { reviewer_note } => {
            assert_eq!(reviewer_note, "fix naming");
        }
        other => panic!("expected Modified, got {:?}", other),
    }
}

// ─── Retry paths ──────────────────────────────────────────────────────────

#[tokio::test]
async fn e2e_retry_then_approve() {
    // Two retries then an approve: all within the max_retries=3 limit.
    let s = drive(
        vec![
            ReviewDecision::Retry {
                new_constraints: "c1".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c2".into(),
            },
            ReviewDecision::Approve,
        ],
        3,
    )
    .await;
    assert!(matches!(s, DelegationStatus::Success));
}

#[tokio::test]
async fn e2e_retry_limit_exceeded() {
    // max_retries=2 means 2 retries are allowed before the limit fires.
    // The check inside run_gate_with_retries is `attempt_n > max_review_retries`
    // (strict `>`), so:
    //   Retry 1 arrives at attempt_n=1 → 1 > 2 false → passes, attempt_n→2
    //   Retry 2 arrives at attempt_n=2 → 2 > 2 false → passes, attempt_n→3
    //   Retry 3 arrives at attempt_n=3 → 3 > 2 true  → Failed
    // The error message reports attempt_n (3), not max_review_retries (2).
    let s = drive(
        vec![
            ReviewDecision::Retry {
                new_constraints: "c1".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c2".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c3".into(),
            },
        ],
        2,
    )
    .await;
    match s {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded"), "got: {}", error);
            // Error reports attempt_n (3), not max_review_retries (2).
            assert!(error.contains('3'), "got: {}", error);
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}

// ─── Timeout path ─────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn e2e_timeout_produces_timed_out() {
    // No decisions submitted — the gate times out after `review_timeout`.
    // `start_paused = true` + `advance` lets us trigger the timeout
    // without sleeping in wall-clock time.
    let sink = ReviewSink::new();
    let sink_for_gate = sink.clone();
    let gate = tokio::spawn(async move {
        run_gate_with_retries(
            ExecutorId::new("e1"),
            DelegationStatus::Success,
            Duration::from_secs(60),
            TimeoutFallback::Reject {
                reason: "review timeout".into(),
            },
            3,
            sink_for_gate,
        )
        .await
    });
    // Yield twice so the gate task registers and enters its select! before
    // we advance virtual time past the timeout.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(120)).await;
    let s = gate.await.unwrap();
    match s {
        DelegationStatus::TimedOut { fallback, .. } => {
            assert!(matches!(fallback, TimeoutFallback::Reject { .. }));
        }
        other => panic!("expected TimedOut, got {:?}", other),
    }
}
