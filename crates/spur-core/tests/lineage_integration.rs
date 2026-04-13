//! End-to-end: simulate a realistic event stream, assert projection matches.

use std::path::PathBuf;

use spur_acp::{
    Artifact, DelegationStatus, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
    SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{ExecutorId, ExecutorLineage};

#[test]
fn full_flow_brain_to_review_to_resolved() {
    let mut l = ExecutorLineage::new();
    // Brain
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    }));
    // Worker spawned via legacy event
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w1".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "w1".into(),
        task: "close the bug".into(),
    }));
    // Executor produces an artifact
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorArtifact {
        id: "w1".into(),
        artifact: Artifact::PrUrl("https://x/42".into()),
    }));
    // Checkpoint: review requested
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "w1".into(),
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "PR ready".into(),
            diff_summary: None,
            pr_url: Some("https://x/42".into()),
            error: None,
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    assert!(n.pending_review.is_some());
    assert_eq!(l.pending_reviews().len(), 1);

    // User approves
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "w1".into(),
        decision: ReviewDecision::Approve,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Success,
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert!(n.pending_review.is_none());
    assert_eq!(n.phase, LifecycleState::Succeeded);
    let a = n.current_attempt().unwrap();
    assert_eq!(a.artifacts.len(), 1);
    assert!(a.ended_at.is_some());
    // Task spec was populated from DelegationRequested
    assert_eq!(n.task_spec, "close the bug");
}

#[test]
fn forest_with_multiple_brains() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "claude".into(),
        session: SessionId("b2".into()),
    }));
    assert_eq!(l.root_ids().len(), 2, "forest should have 2 roots");
}

#[test]
fn retry_preserves_previous_attempts() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "w".into(),
        parent_id: None,
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "initial".into(),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "w".into(),
        phase: LifecycleState::Failed,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 2,
        reason: "transient".into(),
        new_session_id: SessionId("s2".into()),
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 2);
    // Previous attempt preserved with its failed status
    assert!(n.attempts[0].ended_at.is_some());
    // Current attempt is fresh
    assert!(n.attempts[1].ended_at.is_none());
    assert_eq!(n.phase, LifecycleState::Running);
}
