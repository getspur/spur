//! End-to-end: simulate a realistic event stream, assert projection matches.

use std::path::PathBuf;
use std::time::SystemTime;

use spur_acp::{
    DelegationStatus, ExecutorArtifactPayload, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload, SessionId, SpurEvent,
};
use spur_core::{ExecutorId, ExecutorLineage, LifecycleState};

#[test]
fn full_flow_brain_to_review_to_resolved() {
    let mut l = ExecutorLineage::new();
    // Brain
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    });
    // Worker spawned via legacy event
    l.apply(&SpurEvent::WorkerSpawned {
        agent: "w1".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp"),
    });
    l.apply(&SpurEvent::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "w1".into(),
        task: "close the bug".into(),
    });
    // Executor produces an artifact
    l.apply(&SpurEvent::ExecutorArtifact {
        id: "w1".into(),
        artifact: ExecutorArtifactPayload::PrUrl("https://x/42".into()),
    });
    // Checkpoint: review requested
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w1".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "PR ready".into(),
            diff_summary: None,
            pr_url: Some("https://x/42".into()),
            error: None,
        },
        requested_at: SystemTime::now(),
    });

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    assert!(n.pending_review.is_some());
    assert_eq!(l.pending_reviews().len(), 1);

    // User approves
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "w1".into(),
        decision: ExecutorReviewDecision::Approve,
    });
    l.apply(&SpurEvent::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Success,
    });

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
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    });
    l.apply(&SpurEvent::BrainSpawned {
        agent: "claude".into(),
        session: SessionId("b2".into()),
    });
    assert_eq!(l.root_ids().len(), 2, "forest should have 2 roots");
}

#[test]
fn retry_preserves_previous_attempts() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::ExecutorSpawned {
        id: "w".into(),
        parent_id: None,
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: "Executor".into(),
        task_spec: "initial".into(),
    });
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Failed".into(),
    });
    l.apply(&SpurEvent::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 2,
        reason: "transient".into(),
        new_session_id: SessionId("s2".into()),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 2);
    // Previous attempt preserved with its failed status
    assert!(n.attempts[0].ended_at.is_some());
    // Current attempt is fresh
    assert!(n.attempts[1].ended_at.is_none());
    assert_eq!(n.phase, LifecycleState::Running);
}
