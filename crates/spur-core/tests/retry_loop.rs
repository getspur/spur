//! DN-2 regression tests for `RetryLoop`.
//!
//! Invariants asserted here (shared with production and test_support):
//! - 1-indexed attempts.
//! - Strict `>` bound: `max_retries=3` allows attempts 1..=4 to run;
//!   attempt 4's `Retry` outcome triggers the exhaustion path.
//! - Exhaustion returns `DelegationStatus::Failed { error: "retry limit
//!   exceeded after {n} attempts" }` where `n == final attempt_n`.

use spur_acp::DelegationStatus;
use spur_core::retry_loop::{RetryLoop, RetryOutcome};
use std::sync::atomic::{AtomicU32, Ordering};

#[tokio::test]
async fn retry_loop_returns_terminal_on_first_terminal() {
    let rl = RetryLoop::new(3);
    let result = rl
        .run(|_n| async { RetryOutcome::Terminal(DelegationStatus::Success) })
        .await;
    assert!(matches!(result, DelegationStatus::Success));
}

#[tokio::test]
async fn retry_loop_counts_attempts_and_fails_after_limit_plus_one() {
    let rl = RetryLoop::new(3);
    let counter = AtomicU32::new(0);
    let result = rl
        .run(|n| {
            counter.store(n, Ordering::SeqCst);
            async move { RetryOutcome::Retry }
        })
        .await;
    // With max_retries=3, attempts 1, 2, 3, 4 are executed.
    // Attempt 4 returns Retry; `check_exceeded(4, 3)` yields Some(Failed).
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    match result {
        DelegationStatus::Failed { error } => {
            assert!(
                error.contains("retry limit exceeded after 4 attempts"),
                "unexpected error: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn retry_loop_terminal_short_circuits_mid_loop() {
    let rl = RetryLoop::new(5);
    let result = rl
        .run(|n| async move {
            if n == 2 {
                RetryOutcome::Terminal(DelegationStatus::Rejected {
                    reason: "no".into(),
                })
            } else {
                RetryOutcome::Retry
            }
        })
        .await;
    match result {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "no"),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[test]
fn check_exceeded_is_strict_greater_than() {
    // The helper used directly by the production orchestrator retry
    // site. Asserts the exact boundary semantic.
    assert!(RetryLoop::check_exceeded(1, 3).is_none());
    assert!(RetryLoop::check_exceeded(3, 3).is_none()); // at limit, not exceeded
    let exceeded = RetryLoop::check_exceeded(4, 3).expect("4 > 3 should exceed");
    match exceeded {
        DelegationStatus::Failed { error } => {
            assert_eq!(error, "retry limit exceeded after 4 attempts");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
