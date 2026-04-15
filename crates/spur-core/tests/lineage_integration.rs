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
        request_id: "req-1".into(),
        delegation_plan: None,
    }));
    // Executor produces an artifact
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorArtifact {
        id: "w1".into(),
        artifact: Artifact::PrUrl("https://x/42".into()),
    }));
    // Checkpoint: review requested
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "w1".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "PR ready".into(),
            diff_summary: None,
            pr_url: Some("https://x/42".into()),
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    assert!(n.pending_review.is_some());
    assert_eq!(n.pending_review.as_ref().unwrap().attempt_n, 1);
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

#[test]
fn replay_produces_identical_timestamps() {
    use std::time::{Duration, UNIX_EPOCH};

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let events: Vec<SpurEvent> = vec![
        SpurEvent {
            occurred_at: t0,
            seq: 0,
            body: SpurEventBody::ExecutorSpawned {
                id: "w".into(),
                parent_id: None,
                session_id: SessionId("s1".into()),
                agent: "worker".into(),
                role: Role::Executor,
                task_spec: "task".into(),
            },
        },
        SpurEvent {
            occurred_at: t0 + Duration::from_secs(10),
            seq: 0,
            body: SpurEventBody::ExecutorPhaseChanged {
                id: "w".into(),
                phase: LifecycleState::Succeeded,
            },
        },
    ];

    let mut a = ExecutorLineage::new();
    for e in &events {
        a.apply(e);
    }

    std::thread::sleep(Duration::from_millis(10));

    let mut b = ExecutorLineage::new();
    for e in &events {
        b.apply(e);
    }

    let na = a.node(&ExecutorId::new("w")).unwrap();
    let nb = b.node(&ExecutorId::new("w")).unwrap();

    let aa = na.current_attempt().unwrap();
    let ab = nb.current_attempt().unwrap();

    assert_eq!(
        aa.started_at, ab.started_at,
        "started_at must be identical on replay"
    );
    assert_eq!(
        aa.ended_at, ab.ended_at,
        "ended_at must be identical on replay"
    );
    assert_eq!(
        aa.started_at, t0,
        "started_at must come from event.occurred_at"
    );
}

#[test]
fn replay_produces_byte_identical_state() {
    use std::time::{Duration, UNIX_EPOCH};

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mk = |offset_secs: u64, body: SpurEventBody| SpurEvent {
        occurred_at: t0 + Duration::from_secs(offset_secs),
        seq: 0,
        body,
    };

    let events: Vec<SpurEvent> = vec![
        mk(
            0,
            SpurEventBody::BrainSpawned {
                agent: "kiro".into(),
                session: SessionId("b".into()),
            },
        ),
        mk(
            1,
            SpurEventBody::WorkerSpawned {
                agent: "w".into(),
                session: SessionId("w1".into()),
                worktree: PathBuf::from("/tmp"),
            },
        ),
        mk(
            2,
            SpurEventBody::DelegationRequested {
                from: SessionId("b".into()),
                to_agent: "w".into(),
                task: "task".into(),
                request_id: "req-1".into(),
                delegation_plan: None,
            },
        ),
        mk(
            3,
            SpurEventBody::CostUpdate {
                session: SessionId("w1".into()),
                agent: "w".into(),
                estimated_cost_usd: 0.25,
            },
        ),
        mk(
            4,
            SpurEventBody::ExecutorArtifact {
                id: "w1".into(),
                artifact: Artifact::PrUrl("https://x".into()),
            },
        ),
        mk(
            5,
            SpurEventBody::ExecutorReviewRequested {
                id: "w1".into(),
                attempt_n: 1,
                kind: ReviewKind::Completion,
                payload: ReviewPayload {
                    summary: "".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                },
            },
        ),
        mk(
            6,
            SpurEventBody::ExecutorReviewResolved {
                id: "w1".into(),
                decision: ReviewDecision::Approve,
            },
        ),
        mk(
            7,
            SpurEventBody::DelegationCompleted {
                worker_session: SessionId("w1".into()),
                status: DelegationStatus::Success,
            },
        ),
    ];

    let mut a = ExecutorLineage::new();
    for e in &events {
        a.apply(e);
    }

    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut b = ExecutorLineage::new();
    for e in &events {
        b.apply(e);
    }

    #[allow(clippy::type_complexity)]
    let collect = |l: &ExecutorLineage| -> Vec<(
        ExecutorId,
        LifecycleState,
        Vec<(std::time::SystemTime, Option<std::time::SystemTime>)>,
    )> {
        let mut out: Vec<_> = l
            .nodes()
            .map(|n| {
                let attempts: Vec<_> = n
                    .attempts
                    .iter()
                    .map(|a| (a.started_at, a.ended_at))
                    .collect();
                (n.id.clone(), n.phase, attempts)
            })
            .collect();
        out.sort_by(|x, y| x.0 .0.cmp(&y.0 .0));
        out
    };

    assert_eq!(
        collect(&a),
        collect(&b),
        "replay must produce identical state including timestamps"
    );
}

#[test]
fn applying_same_event_twice_is_idempotent_except_cost() {
    use std::time::{Duration, UNIX_EPOCH};
    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let spawn = SpurEvent {
        occurred_at: t0,
        seq: 0,
        body: SpurEventBody::ExecutorSpawned {
            id: "w".into(),
            parent_id: None,
            session_id: SessionId("s".into()),
            agent: "a".into(),
            role: Role::Brain,
            task_spec: "".into(),
        },
    };
    let phase = SpurEvent {
        occurred_at: t0 + Duration::from_secs(1),
        seq: 0,
        body: SpurEventBody::ExecutorPhaseChanged {
            id: "w".into(),
            phase: LifecycleState::Running,
        },
    };

    let mut l = ExecutorLineage::new();
    l.apply(&spawn);
    l.apply(&phase);
    l.apply(&spawn);
    l.apply(&phase); // re-apply — idempotent

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(
        n.attempts.len(),
        1,
        "duplicate spawn must not create new node/attempt"
    );
    assert_eq!(n.phase, LifecycleState::Running);
}

// ─── Fix 2: adapter renders Rejected/Modified/TimedOut with real semantics ──

#[test]
fn delegation_completed_modified_renders_as_succeeded_with_note() {
    // Modified is human-approved-with-note — must map to Succeeded, NOT Failed.
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w1".into()),
        worktree: std::path::PathBuf::from("/tmp/wt"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Modified {
            reviewer_note: "fix the naming".to_string(),
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(
        n.phase,
        LifecycleState::Succeeded,
        "Modified must map to LifecycleState::Succeeded (human approved-with-note)"
    );
    let a = n.current_attempt().unwrap();
    assert!(
        a.error
            .as_deref()
            .map(|e| e.contains("fix the naming"))
            .unwrap_or(false),
        "adapter must carry the reviewer note into attempt.error, got: {:?}",
        a.error
    );
}

#[test]
fn delegation_completed_rejected_renders_as_failed_with_reason() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w1".into()),
        worktree: std::path::PathBuf::from("/tmp/wt"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Rejected {
            reason: "out of scope".to_string(),
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Failed);
    let a = n.current_attempt().unwrap();
    assert!(
        a.error
            .as_deref()
            .map(|e| e.contains("out of scope"))
            .unwrap_or(false),
        "adapter must carry rejection reason into attempt.error, got: {:?}",
        a.error
    );
}

#[test]
fn delegation_completed_timed_out_renders_as_failed_with_timeout_detail() {
    use spur_acp::TimeoutFallback;
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w1".into()),
        worktree: std::path::PathBuf::from("/tmp/wt"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::TimedOut {
            waited_for: std::time::Duration::from_secs(1800),
            fallback: TimeoutFallback::Abandon,
        },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Failed);
    let a = n.current_attempt().unwrap();
    assert!(
        a.error
            .as_deref()
            .map(|e| e.contains("1800"))
            .unwrap_or(false),
        "adapter must include wait duration in attempt.error, got: {:?}",
        a.error
    );
}
