//! Verifies new executor lineage events round-trip through serde JSON.

use spur_acp::{
    ExecutorArtifactPayload, ExecutorReviewDecision, ExecutorReviewKind, ExecutorReviewPayload,
    SessionId, SpurEvent,
};

#[test]
fn executor_spawned_roundtrips() {
    let ev = SpurEvent::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: Some("brain-1".into()),
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: "Executor".into(),
        task_spec: "fix bug".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round, SpurEvent::ExecutorSpawned { .. }));
}

#[test]
fn executor_review_resolved_roundtrips() {
    let ev = SpurEvent::ExecutorReviewResolved {
        id: "exec-1".into(),
        decision: ExecutorReviewDecision::Reject {
            reason: "tests fail".into(),
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round, SpurEvent::ExecutorReviewResolved { .. }));
}
