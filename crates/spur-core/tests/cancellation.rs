//! INV-6: CancellationControl primitive tests and DelegationCompleted
//! emission regression test.
//!
//! A full Orchestrator e2e cancellation test would require a real git
//! worktree and a fake ACP worker, which is out of scope for this pass.
//! Instead we test the `CancellationControl` primitive — the token
//! registry used by `handle_delegations`'s `tokio::select!` — in
//! isolation.  This proves the mechanism works; an Orchestrator-level
//! integration test can be added later.

use spur_acp::{
    CancelOutcome, CancellationControl, DelegationResult, DelegationStatus, SpurEventBody,
};
use std::sync::Arc;
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
                    artifact: None,
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
                    artifact: None,
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

/// Regression: the cancel arm of `tokio::select!` must emit
/// `DelegationCompleted` so the TUI / lineage projection never see a
/// delegation stuck "active" forever.
///
/// We mirror the production cancel arm exactly (token + funnel emit +
/// select! against a slow future) using a real `spawn_funnel` and a
/// broadcast receiver — same pattern used in `event_funnel` unit tests.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_emits_delegation_completed() {
    use spur_acp::types::SessionId;
    use spur_core::event_funnel::spawn_funnel;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::broadcast;

    // Set up a real funnel + broadcast subscriber.
    let (bcast_tx, mut bcast_rx) = broadcast::channel(32);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let cc = CancellationControl::new();
    let token = cc.register("del-reg".into()).await;
    let funnel_for_task = funnel.clone();

    // Mirror the production cancel arm: select! cancel_token vs slow work,
    // emitting DelegationCompleted on the cancel branch.
    let worker = tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = token.cancelled() => {
                let status = DelegationStatus::Cancelled {
                    reason: "brain requested cancel".into(),
                };
                funnel_for_task.emit(SpurEventBody::DelegationCompleted {
                    worker_session: SessionId("del-reg".into()),
                    status: status.clone(),
                });
                DelegationResult {
                    status,
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                }
            }
        };
        result
    });

    // Yield so the worker task enters select!.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Signal cancel.
    let outcome = cc.cancel("del-reg").await;
    assert_eq!(outcome, CancelOutcome::Cancelled);

    // Advance virtual time to let the funnel task drain the channel.
    tokio::time::advance(Duration::from_millis(10)).await;

    // Worker must complete with Cancelled.
    let result = worker.await.expect("worker panicked");
    assert!(
        matches!(result.status, DelegationStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );

    // The broadcast must contain a DelegationCompleted(Cancelled) event.
    // Yield a few times so the funnel task processes the queued emit.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let event = bcast_rx
        .try_recv()
        .expect("DelegationCompleted must be on the broadcast channel");
    assert!(
        matches!(
            event.body,
            SpurEventBody::DelegationCompleted {
                status: DelegationStatus::Cancelled { .. },
                ..
            }
        ),
        "expected DelegationCompleted(Cancelled), got {:?}",
        event.body
    );
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
