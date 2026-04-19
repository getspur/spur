//! INV-6: CancellationControl primitive tests.
//!
//! A full Orchestrator e2e cancellation test would require a real git
//! worktree and a fake ACP worker, which is out of scope for this pass.
//! Instead we test the `CancellationControl` primitive — the token
//! registry used by `handle_delegations`'s `tokio::select!` — in
//! isolation.  This proves the mechanism works; an Orchestrator-level
//! integration test can be added later (see DONE_WITH_CONCERNS note in
//! the task report).

use spur_acp::{CancelOutcome, CancellationControl, DelegationResult, DelegationStatus};
use std::time::Duration;

/// Register a token, race execute_delegation look-alike against it,
/// signal cancel — verify the select! arm yields Cancelled status.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_wins_over_slow_worker() {
    let cc = CancellationControl::new();

    // Simulate what handle_delegations does: register token before spawning.
    let token = cc.register("delegation-abc".into()).await;

    // Spawn a "slow worker" task that returns only after 60 virtual seconds.
    let worker_handle = tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = token.cancelled() => {
                DelegationResult {
                    status: DelegationStatus::Cancelled {
                        reason: "brain requested cancel".into(),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: Some("completed normally".into()),
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                }
            }
        };
        result
    });

    // Yield so the spawned task is polled and enters select!.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Cancel before 60s elapses — should win the select!.
    let outcome = cc.cancel("delegation-abc").await;
    assert_eq!(outcome, CancelOutcome::Cancelled);

    // Advance virtual time slightly (cancel should already have fired).
    tokio::time::advance(Duration::from_millis(100)).await;

    let result = worker_handle.await.expect("worker task panicked");
    assert!(
        matches!(result.status, DelegationStatus::Cancelled { .. }),
        "expected Cancelled status, got {:?}",
        result.status
    );

    // Token entry was removed by cancel() — a second call is NotFound.
    let second = cc.cancel("delegation-abc").await;
    assert_eq!(second, CancelOutcome::NotFound);
}

/// Normal completion removes the token so a subsequent cancel call is NotFound.
#[tokio::test(flavor = "current_thread")]
async fn normal_completion_removes_token() {
    let cc = CancellationControl::new();
    cc.register("delegation-xyz".into()).await;

    // Simulate normal task completion: remove token without cancelling.
    cc.remove("delegation-xyz").await;

    // After normal completion, cancel returns NotFound (already cleaned up).
    let outcome = cc.cancel("delegation-xyz").await;
    assert_eq!(outcome, CancelOutcome::NotFound);
}
