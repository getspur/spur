//! Regression tests for bugs found in final code review:
//! - Esc after JumpToReview should unfocus, not quit.
//! - SubmitReview on node without pending_review must not fire wire event.
//! - JumpToReview must set detail tab to Review.
//! - pending_reviews must return insertion order.

use std::time::SystemTime;

use spur_acp::{
    ExecutorReviewKind, ExecutorReviewPayload, SessionId, SpurEvent,
};
use spur_core::{ExecutorId, ExecutorLineage};

fn spawn(id: &str) -> SpurEvent {
    SpurEvent::ExecutorSpawned {
        id: id.into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: id.into(),
        role: "Brain".into(),
        task_spec: String::new(),
    }
}

fn review_req(id: &str) -> SpurEvent {
    SpurEvent::ExecutorReviewRequested {
        id: id.into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "s".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
        requested_at: SystemTime::now(),
    }
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
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "a".into(),
        decision: spur_acp::ExecutorReviewDecision::Approve,
    });
    let order = l.pending_reviews();
    assert_eq!(order, vec![ExecutorId::new("b")]);
}

#[test]
fn resolving_nonexistent_review_is_noop() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("a"));
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "a".into(),
        decision: spur_acp::ExecutorReviewDecision::Approve,
    });
    // No panic; state unchanged
    assert_eq!(l.pending_reviews().len(), 0);
}
