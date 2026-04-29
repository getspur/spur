use std::path::PathBuf;

use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{MessageKind, PeerMessageId};
use spur_acp::{
    Artifact, DelegationStatus, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
    SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{AttemptStatus, ExecutorId, ExecutorLineage};

fn spawn(id: &str, parent: Option<&str>) -> SpurEvent {
    SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: id.into(),
        parent_id: parent.map(|s| s.into()),
        session_id: SessionId(format!("sess-{}", id)),
        agent: "kiro".into(),
        role: if parent.is_none() {
            Role::Brain
        } else {
            Role::Executor
        },
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
fn lineage_peer_message_accepted_creates_edge_on_source_node() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("src", None));

    let message_id = PeerMessageId(uuid::Uuid::new_v4());
    l.apply(&SpurEvent::now(SpurEventBody::WorkerPeerMessageAccepted {
        brain_session_id: "brain".into(),
        message_id,
        source_delegation_id: DelegationId("src".into()),
        target_delegation_id: DelegationId("tgt".into()),
        kind: MessageKind::Handoff,
        sequence: 1,
    }));

    let edges = l.peer_edges_for_delegation(&DelegationId("src".into()));
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].message_id, message_id);
    assert_eq!(edges[0].source_delegation_id, DelegationId("src".into()));
    assert_eq!(edges[0].target_delegation_id, DelegationId("tgt".into()));
    assert_eq!(edges[0].kind, MessageKind::Handoff);
}

#[test]
fn lineage_finds_peer_edges_inbound_to_target_delegation() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("src-a", None));
    l.apply(&spawn("src-b", None));

    let target = DelegationId("target".into());
    let first = PeerMessageId(uuid::Uuid::new_v4());
    let second = PeerMessageId(uuid::Uuid::new_v4());
    l.apply(&SpurEvent::now(SpurEventBody::WorkerPeerMessageAccepted {
        brain_session_id: "brain".into(),
        message_id: first,
        source_delegation_id: DelegationId("src-a".into()),
        target_delegation_id: target.clone(),
        kind: MessageKind::Handoff,
        sequence: 1,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerPeerMessageAccepted {
        brain_session_id: "brain".into(),
        message_id: second,
        source_delegation_id: DelegationId("src-b".into()),
        target_delegation_id: target.clone(),
        kind: MessageKind::Question,
        sequence: 2,
    }));

    let inbound = l.peer_edges_inbound_for_delegation(&target);
    let mut ids: Vec<_> = inbound.iter().map(|edge| edge.message_id).collect();
    ids.sort_by_key(|id| id.0);

    let mut expected = vec![first, second];
    expected.sort_by_key(|id| id.0);
    assert_eq!(ids, expected);
    assert!(l.peer_edges_for_delegation(&target).is_empty());
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
    assert!(
        a.ended_at.is_some(),
        "terminal phase must close the attempt"
    );
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
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_some());
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    let r = n.pending_review.as_ref().unwrap();
    assert_eq!(
        r.attempt_n, 1,
        "attempt_n must be carried into ReviewRequest"
    );
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
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
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
fn delegation_completed_cancelled_moves_phase_to_cancelled_with_reason() {
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
        status: DelegationStatus::Cancelled {
            reason: "brain requested cancel".into(),
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Cancelled);

    let a = n.current_attempt().unwrap();
    assert_eq!(a.status, AttemptStatus::Cancelled);
    assert!(a.ended_at.is_some());
    assert_eq!(a.error.as_deref(), Some("brain requested cancel"));
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
            request_id: "req-1".into(),
            delegation_plan: None,
            issue_id: None,
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
    for e in &events {
        live.apply(e);
    }

    let mut replayed = ExecutorLineage::new();
    for e in &events {
        replayed.apply(e);
    }

    let a: Vec<_> = live
        .nodes()
        .map(|n| (n.id.clone(), n.phase, n.task_spec.clone()))
        .collect();
    let b: Vec<_> = replayed
        .nodes()
        .map(|n| (n.id.clone(), n.phase, n.task_spec.clone()))
        .collect();
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
    assert!(
        l.node(&ExecutorId::new("child")).is_none(),
        "child should be buffered, not attached as root"
    );
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
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
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
    let pending = lineage.pending_reviews();
    assert!(
        !pending.iter().any(|e| e.0 == "exec-1"),
        "cancelled executor must be removed from pending_review_order"
    );
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

// ─── BrainRetired fold ────────────────────────────────────────────────
//
// Corresponds to `retire_active_brain` in the orchestrator: the retired
// brain and all non-terminal descendants move to `Cancelled`, attempts
// close with `ended_at = event.occurred_at`, and pending-review queue
// is drained for cascaded ids.

use spur_acp::domain::events::BrainRetireReason;

#[test]
fn brain_retired_cascades_descendants_to_cancelled() {
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

    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));

    let brain = l.node(&ExecutorId::new("b1")).unwrap();
    assert_eq!(
        brain.phase,
        LifecycleState::Cancelled,
        "brain must move to Cancelled"
    );
    assert!(
        brain.current_attempt().unwrap().ended_at.is_some(),
        "brain attempt must close"
    );

    let worker = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(
        worker.phase,
        LifecycleState::Cancelled,
        "descendant must cascade to Cancelled"
    );
    assert!(
        worker.current_attempt().unwrap().ended_at.is_some(),
        "descendant attempt must close"
    );
}

#[test]
fn brain_retired_preserves_terminal_descendants() {
    // A child that already succeeded must not be downgraded to Cancelled.
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

    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));

    let worker = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(
        worker.phase,
        LifecycleState::Succeeded,
        "already-terminal descendant must not be overwritten"
    );
}

#[test]
fn brain_retired_drains_pending_review_for_cascaded_ids() {
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
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "w1".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));
    assert_eq!(l.pending_reviews().len(), 1);

    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));

    assert!(
        l.pending_reviews().is_empty(),
        "cascaded descendants must be drained from pending_review_order"
    );
    let worker = l.node(&ExecutorId::new("w1")).unwrap();
    assert!(worker.pending_review.is_none());
}

#[test]
fn brain_retired_is_idempotent() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    let ev = SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    });
    l.apply(&ev);
    let after_first = l.node(&ExecutorId::new("b1")).unwrap().clone();

    // Apply same event again — must be a no-op.
    l.apply(&ev);
    let after_second = l.node(&ExecutorId::new("b1")).unwrap().clone();

    assert_eq!(after_first.phase, after_second.phase);
    assert_eq!(
        after_first.current_attempt().unwrap().ended_at,
        after_second.current_attempt().unwrap().ended_at,
        "ended_at must not drift on repeated apply"
    );
}

#[test]
fn two_clear_cycles_leave_one_running_root() {
    // Regression: before BrainRetired, each /clear left a zombie root in
    // Running. With the fold arm, only the latest brain is non-terminal.
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b2".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b2".into()),
        reason: BrainRetireReason::UserClear,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b3".into()),
    }));

    assert_eq!(l.root_ids().len(), 3, "all three roots retained");
    let running: Vec<_> = l
        .root_ids()
        .iter()
        .filter(|id| {
            l.node(id)
                .map(|n| n.phase)
                .unwrap_or(LifecycleState::Spawning)
                != LifecycleState::Cancelled
        })
        .collect();
    assert_eq!(running.len(), 1, "only the latest brain stays non-terminal");
    assert_eq!(running[0], &ExecutorId::new("b3"));
}

#[test]
fn worker_spawn_after_retire_attaches_under_live_brain_not_zombie() {
    // Adapter parent-inference must skip terminal Brain roots.
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b2".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w-late".into()),
        worktree: PathBuf::from("/tmp/wt"),
    }));

    let w = l.node(&ExecutorId::new("w-late")).unwrap();
    assert_eq!(
        w.parent_id.as_ref().unwrap(),
        &ExecutorId::new("b2"),
        "worker must attach under the live brain, not the retired one"
    );
    let zombie = l.node(&ExecutorId::new("b1")).unwrap();
    assert!(
        zombie.child_ids.is_empty(),
        "retired brain must not receive new children"
    );
}

#[test]
fn brain_retired_before_spawn_is_buffered_as_orphan() {
    // If BrainRetired arrives before BrainSpawned (pathological ordering,
    // e.g. replay from a partial log), it must be buffered and replayed.
    // This guarantees we never silently drop a close-out event.
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));
    // No node exists yet — nothing to assert beyond "no panic".
    assert!(l.node(&ExecutorId::new("b1")).is_none());

    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    let n = l.node(&ExecutorId::new("b1")).unwrap();
    assert_eq!(
        n.phase,
        LifecycleState::Cancelled,
        "orphan-buffered BrainRetired must replay after spawn"
    );
}
