use tokio::sync::mpsc;

use crate::lineage::ExecutorId;
use crate::review_sink::ReviewSink;
use spur_acp::SpurEventBody;

use super::input::InteractiveInput;

/// Emit `ExecutorReviewCancelled` and remove the sink entry.
///
/// Called from the brain-cancellation path — when `respond_to.send(result)` returns
/// `Err`, the brain has gone away, and any pending review for this delegation must
/// be recorded in the lineage projection as abandoned (otherwise the TUI shows an
/// orphaned review card indefinitely).
///
/// Idempotent: if no review is registered, `review_sink.remove` is a no-op, and the
/// event is still emitted so the lineage projection records the cancellation.
pub async fn cleanup_cancelled_review(
    executor_id: &ExecutorId,
    reason: &str,
    funnel: &crate::event_funnel::FunnelHandle,
    review_sink: &ReviewSink,
) {
    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
        id: executor_id.0.clone(),
        reason: reason.to_string(),
    });
    review_sink.remove(executor_id).await;
}

/// Dispatcher loop: forwards `SubmitReview` messages to the `ReviewSink`.
/// All other `InteractiveInput` variants are ignored by this loop (they
/// are consumed by `run_interactive`'s own loop, not this one).
///
/// This is spawned as a separate task so review-decision latency is
/// decoupled from brain-turn I/O latency — see spec "Unit 3" for
/// rationale.
pub async fn review_dispatcher_loop(mut rx: mpsc::Receiver<InteractiveInput>, sink: ReviewSink) {
    while let Some(input) = rx.recv().await {
        if let InteractiveInput::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        } = input
        {
            let _ = sink
                .submit(ExecutorId::new(executor_id), attempt_n, decision)
                .await;
        }
        // All other variants: noop in this loop.
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn apply_decision_to_candidate(
    decision: spur_acp::ReviewDecision,
    candidate: spur_acp::DelegationStatus,
) -> spur_acp::DelegationStatus {
    use spur_acp::DelegationStatus;
    use spur_acp::ReviewDecision;
    match decision {
        ReviewDecision::Approve => candidate,
        ReviewDecision::Reject { reason } => DelegationStatus::Rejected { reason },
        ReviewDecision::Modify { note } => DelegationStatus::Modified {
            reviewer_note: note,
        },
        ReviewDecision::Retry { .. } => DelegationStatus::Failed {
            error: "internal: Retry reached run_gate_for_candidate \
                    (caller must wrap with retry loop)"
                .into(),
        },
    }
}
