use std::time::SystemTime;

use spur_acp::{
    ExecutorArtifactPayload, ExecutorReviewDecision, ExecutorReviewKind, ExecutorReviewPayload,
    SessionId, SpurEvent,
};
use spur_core::{Artifact, ExecutorId, ExecutorLineage, LifecycleState};

fn spawn(id: &str, parent: Option<&str>) -> SpurEvent {
    SpurEvent::ExecutorSpawned {
        id: id.into(),
        parent_id: parent.map(|s| s.into()),
        session_id: SessionId(format!("sess-{}", id)),
        agent: "kiro".into(),
        role: if parent.is_none() {
            "Brain".into()
        } else {
            "Executor".into()
        },
        task_spec: format!("task for {}", id),
    }
}

#[test]
fn spawn_creates_root_when_no_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));

    assert_eq!(l.root_ids().len(), 1);
    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert!(n.parent_id.is_none());
    assert_eq!(n.phase, LifecycleState::Spawning);
    assert_eq!(n.attempts.len(), 1);
}

#[test]
fn spawn_links_child_under_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&spawn("worker-1", Some("brain-1")));

    assert_eq!(l.root_ids().len(), 1);
    let parent = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(parent.child_ids.len(), 1);
    assert_eq!(parent.child_ids[0], ExecutorId::new("worker-1"));

    let child = l.node(&ExecutorId::new("worker-1")).unwrap();
    assert_eq!(child.parent_id, Some(ExecutorId::new("brain-1")));
}

#[test]
fn phase_change_updates_node_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "brain-1".into(),
        phase: "Running".into(),
    });

    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn phase_change_terminal_sets_attempt_ended() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Succeeded".into(),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!(a.ended_at.is_some(), "terminal phase must close the attempt");
}

#[test]
fn unknown_phase_string_is_ignored() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Bogus".into(),
    });
    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.phase, LifecycleState::Spawning, "unchanged on unknown phase");
}

#[test]
fn artifact_appends_to_current_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorArtifact {
        id: "w".into(),
        artifact: ExecutorArtifactPayload::PrUrl("https://x/1".into()),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert_eq!(a.artifacts.len(), 1);
    assert!(matches!(a.artifacts[0], Artifact::PrUrl(_)));
}

#[test]
fn review_requested_populates_pending_review_and_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
        requested_at: SystemTime::now(),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_some());
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
}

#[test]
fn review_resolved_clears_pending_review() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
        requested_at: SystemTime::now(),
    });
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "w".into(),
        decision: ExecutorReviewDecision::Approve,
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_none());
}

#[test]
fn retry_started_pushes_new_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Failed".into(),
    });
    l.apply(&SpurEvent::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 2,
        reason: "timeout".into(),
        new_session_id: SessionId("sess-w-2".into()),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 2);
    assert_eq!(n.phase, LifecycleState::Running);
    assert_eq!(n.current_attempt().unwrap().session_id.0, "sess-w-2");
}

#[test]
fn orphan_phase_event_is_replayed_after_spawn() {
    let mut l = ExecutorLineage::new();
    // Phase arrives BEFORE spawn — must be buffered.
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "late".into(),
        phase: "Running".into(),
    });
    assert!(l.node(&ExecutorId::new("late")).is_none());

    l.apply(&spawn("late", None));
    let n = l.node(&ExecutorId::new("late")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}
