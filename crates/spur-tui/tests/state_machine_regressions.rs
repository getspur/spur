//! Regression tests for bugs found in final code review:
//! - Esc after JumpToReview should unfocus, not quit.
//! - SubmitReview on node without pending_review must not fire wire event.
//! - JumpToReview must set detail tab to Review.
//! - pending_reviews must return insertion order.

use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
use spur_core::{ExecutorId, ExecutorLineage};

fn spawn(id: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: id.into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: id.into(),
        role: Role::Brain,
        task_spec: String::new(),
    })
}

fn review_req(id: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: id.into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "s".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
        },
    })
}

#[test]
fn pending_reviews_returns_insertion_order() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("a"));
    l.apply(&spawn("b"));
    l.apply(&spawn("c"));
    l.apply(&review_req("b"));
    l.apply(&review_req("a"));
    l.apply(&review_req("c"));

    let order = l.pending_reviews();
    assert_eq!(
        order,
        vec![
            ExecutorId::new("b"),
            ExecutorId::new("a"),
            ExecutorId::new("c"),
        ]
    );
}

#[test]
fn pending_reviews_removes_resolved_entries() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("a"));
    l.apply(&spawn("b"));
    l.apply(&review_req("a"));
    l.apply(&review_req("b"));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "a".into(),
        decision: spur_acp::ReviewDecision::Approve,
    }));
    let order = l.pending_reviews();
    assert_eq!(order, vec![ExecutorId::new("b")]);
}

#[test]
fn resolving_nonexistent_review_is_noop() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("a"));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "a".into(),
        decision: spur_acp::ReviewDecision::Approve,
    }));
    // No panic; state unchanged
    assert_eq!(l.pending_reviews().len(), 0);
}
