use std::path::PathBuf;

use spur_acp::{
    Artifact, DelegationStatus, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
    SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{ExecutorId, ExecutorLineage};

fn spawn(id: &str, parent: Option<&str>) -> SpurEvent {
    SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: id.into(),
        parent_id: parent.map(|s| s.into()),
        session_id: SessionId(format!("sess-{}", id)),
        agent: "kiro".into(),
        role: if parent.is_none() { Role::Brain } else { Role::Executor },
        task_spec: format!("task for {}", id),
    })
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
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "brain-1".into(),
        phase: LifecycleState::Running,
    }));

    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn phase_change_terminal_sets_attempt_ended() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "w".into(),
        phase: LifecycleState::Succeeded,
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!(a.ended_at.is_some(), "terminal phase must close the attempt");
}

#[test]
fn artifact_appends_to_current_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorArtifact {
        id: "w".into(),
        artifact: Artifact::PrUrl("https://x/1".into()),
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert_eq!(a.artifacts.len(), 1);
    assert!(matches!(a.artifacts[0], Artifact::PrUrl(_)));
}

#[test]
fn review_requested_populates_pending_review_and_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "w".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_some());
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    let r = n.pending_review.as_ref().unwrap();
    assert_eq!(r.attempt_n, 1, "attempt_n must be carried into ReviewRequest");
}

#[test]
fn review_resolved_clears_pending_review() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "w".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "w".into(),
        decision: ReviewDecision::Approve,
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_none());
    assert_eq!(
        n.phase,
        LifecycleState::AwaitingReview,
        "resolve must not change phase"
    );
}

#[test]
fn retry_started_pushes_new_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "w".into(),
        phase: LifecycleState::Failed,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 2,
        reason: "timeout".into(),
        new_session_id: SessionId("sess-w-2".into()),
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 2);
    assert_eq!(n.phase, LifecycleState::Running);
    assert_eq!(n.current_attempt().unwrap().session_id.0, "sess-w-2");
}

#[test]
fn orphan_phase_event_is_replayed_after_spawn() {
    let mut l = ExecutorLineage::new();
    // Phase arrives BEFORE spawn — must be buffered.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "late".into(),
        phase: LifecycleState::Running,
    }));
    assert!(l.node(&ExecutorId::new("late")).is_none());

    l.apply(&spawn("late", None));
    let n = l.node(&ExecutorId::new("late")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn brain_spawned_creates_root_node() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("s1".into()),
    }));
    assert_eq!(l.root_ids().len(), 1);
    assert!(l.node(&ExecutorId::new("s1")).is_some());
}

#[test]
fn worker_spawned_attaches_under_latest_brain() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "worker".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp/wt"),
    }));

    let brain = l.node(&ExecutorId::new("b1")).unwrap();
    assert_eq!(brain.child_ids.len(), 1);
    assert_eq!(brain.child_ids[0], ExecutorId::new("w1"));
}

#[test]
fn delegation_completed_success_moves_phase_to_succeeded() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp/wt"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Success,
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Succeeded);
}

#[test]
fn cost_update_accumulates_on_current_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::CostUpdate {
        session: SessionId("b1".into()),
        agent: "kiro".into(),
        estimated_cost_usd: 0.10,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::CostUpdate {
        session: SessionId("b1".into()),
        agent: "kiro".into(),
        estimated_cost_usd: 0.05,
    }));

    let n = l.node(&ExecutorId::new("b1")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!((a.cost_usd - 0.15).abs() < 1e-9);
}

#[test]
fn replay_equals_live() {
    let events: Vec<SpurEvent> = vec![
        SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b".into()),
        }),
        SpurEvent::now(SpurEventBody::WorkerSpawned {
            agent: "w1".into(),
            session: SessionId("w1".into()),
            worktree: PathBuf::from("/tmp"),
        }),
        SpurEvent::now(SpurEventBody::DelegationRequested {
            from: SessionId("b".into()),
            to_agent: "w1".into(),
            task: "task-1".into(),
        }),
        SpurEvent::now(SpurEventBody::CostUpdate {
            session: SessionId("w1".into()),
            agent: "w1".into(),
            estimated_cost_usd: 0.25,
        }),
        SpurEvent::now(SpurEventBody::DelegationCompleted {
            worker_session: SessionId("w1".into()),
            status: DelegationStatus::Success,
        }),
    ];

    let mut live = ExecutorLineage::new();
    for e in &events { live.apply(e); }

    let mut replayed = ExecutorLineage::new();
    for e in &events { replayed.apply(e); }

    let a: Vec<_> = live.nodes().map(|n| (n.id.clone(), n.phase, n.task_spec.clone())).collect();
    let b: Vec<_> = replayed.nodes().map(|n| (n.id.clone(), n.phase, n.task_spec.clone())).collect();
    assert_eq!(a.len(), b.len());
    for x in &a {
        assert!(b.contains(x), "replayed state missing {:?}", x);
    }
}

#[test]
fn child_spawn_before_parent_spawn_attaches_on_parent_arrival() {
    let mut l = ExecutorLineage::new();

    // Child arrives FIRST — names parent that doesn't yet exist.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "child".into(),
        parent_id: Some("parent".into()),
        session_id: SessionId("s-child".into()),
        agent: "c".into(),
        role: Role::Executor,
        task_spec: "".into(),
    }));

    // Before parent exists, child must NOT be a root.
    assert!(l.node(&ExecutorId::new("child")).is_none(),
            "child should be buffered, not attached as root");
    assert_eq!(l.root_ids().len(), 0);

    // Parent arrives.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "parent".into(),
        parent_id: None,
        session_id: SessionId("s-parent".into()),
        agent: "p".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));

    // Now both exist and child is attached under parent.
    let p = l.node(&ExecutorId::new("parent")).unwrap();
    let c = l.node(&ExecutorId::new("child")).unwrap();
    assert_eq!(p.child_ids.len(), 1);
    assert_eq!(p.child_ids[0], ExecutorId::new("child"));
    assert_eq!(c.parent_id, Some(ExecutorId::new("parent")));
    assert_eq!(l.root_ids().len(), 1, "only parent is a root");
}

#[test]
fn attempt_n_mismatch_still_appends_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "w".into(),
        parent_id: None,
        session_id: SessionId("s1".into()),
        agent: "w".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));
    // Skip to attempt 5 — orchestrator dropped retry events 2..4.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 5,
        reason: "drop".into(),
        new_session_id: SessionId("s5".into()),
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    // Still appends — validation is observability-only.
    assert_eq!(n.attempts.len(), 2, "retry appends even on mismatch");
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn review_cancelled_clears_pending_review() {
    use spur_acp::{ReviewKind, ReviewPayload, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ExecutorLineage};
    let mut lineage = ExecutorLineage::default();
    // Spawn + request review first (uses existing helpers if present; else construct events inline).
    let spawn = SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: None,
        session_id: spur_acp::SessionId::new(),
        agent: "worker".into(),
        role: spur_acp::Role::Executor,
        task_spec: "t".into(),
    });
    lineage.apply(&spawn);
    let req = SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    });
    lineage.apply(&req);
    assert!(lineage
        .node(&ExecutorId("exec-1".into()))
        .unwrap()
        .pending_review
        .is_some());

    let cancel = SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: "exec-1".into(),
        reason: "brain cancel".into(),
    });
    lineage.apply(&cancel);
    assert!(lineage
        .node(&ExecutorId("exec-1".into()))
        .unwrap()
        .pending_review
        .is_none());
}

#[test]
fn orphan_replay_does_not_retrigger_legacy_adapter() {
    // Buffer a child-orphan phase change, then trigger spawn. Legacy adapter
    // must NOT fire on the replay (today's apply_legacy is a no-op for
    // Executor* variants, but a future change must not break this).
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "x".into(),
        phase: LifecycleState::Running,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "x".into(),
        parent_id: None,
        session_id: SessionId("s".into()),
        agent: "a".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));

    let n = l.node(&ExecutorId::new("x")).unwrap();
    // One attempt only — no legacy-path duplicate spawn.
    assert_eq!(n.attempts.len(), 1);
    assert_eq!(n.phase, LifecycleState::Running);
}
