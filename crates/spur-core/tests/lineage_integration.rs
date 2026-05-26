//! End-to-end: simulate a realistic event stream, assert projection matches.

use std::path::PathBuf;

use spur_acp::{
    Artifact, AttemptSetupError, DelegationStatus, LifecycleState, ReviewDecision, ReviewKind,
    ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{AttemptStatus, ExecutorId, ExecutorLineage};

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
        issue_id: None,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("b".into()),
        request_id: "req-1".into(),
        executor_id: "w1".into(),
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
            peer_influence: None,
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
                issue_id: None,
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
                    peer_influence: None,
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
fn delegation_completed_setup_failed_renders_as_failed_with_setup_error() {
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

    let setup_error = AttemptSetupError::OverlayConflict {
        source_task_id: "task-1".to_string(),
        files: vec!["src/lib.rs".to_string()],
    };
    let expected_error = setup_error.to_string();
    l.apply(&SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::SetupFailed { error: setup_error },
    }));

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Failed);
    assert_eq!(n.last_error.as_deref(), Some(expected_error.as_str()));
    let a = n.current_attempt().unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.error.as_deref(), Some(expected_error.as_str()));
    assert!(
        a.error
            .as_deref()
            .map(|e| e.contains("overlay"))
            .unwrap_or(false),
        "adapter must carry setup error into attempt.error, got: {:?}",
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

#[test]
fn concurrent_same_agent_workers_attribute_tasks_correctly() {
    // Two coder workers dispatched near-simultaneously. DelegationDispatched
    // carries (request_id -> executor_id) mapping. task_spec MUST land on
    // the executor matched by request_id, not by agent name.
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_core::lineage::{ExecutorId, ExecutorLineage};

    let mut l = ExecutorLineage::default();

    // Spawn both executors (WorkerSpawned path creates them with empty task_spec).
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-A".into()),
        worktree: std::path::PathBuf::from("/tmp/wA"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-B".into()),
        worktree: std::path::PathBuf::from("/tmp/wB"),
    }));

    // DelegationRequested for task A arrives first (buffered by request_id).
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "coder".into(),
        task: "TASK-A: fix login CSS".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));
    // DelegationRequested for task B arrives.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "coder".into(),
        task: "TASK-B: add rate limiter".into(),
        request_id: "req-B".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    // Dispatch events arrive out of order — B before A.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-B".into(),
        executor_id: "worker-B".into(),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-A".into(),
        executor_id: "worker-A".into(),
    }));

    // Assertions: each node carries the task matched by request_id.
    let node_a = l.node(&ExecutorId::new("worker-A")).expect("worker-A");
    let node_b = l.node(&ExecutorId::new("worker-B")).expect("worker-B");
    assert_eq!(
        node_a.task_spec, "TASK-A: fix login CSS",
        "worker-A must carry task A (matched by request_id req-A)"
    );
    assert_eq!(
        node_b.task_spec, "TASK-B: add rate limiter",
        "worker-B must carry task B (matched by request_id req-B)"
    );
}

#[test]
fn eager_stamp_cannot_misattribute_when_dispatch_order_differs() {
    // Adversarial order: one executor exists when DelegationRequested arrives,
    // so any agent-name eager-stamp would fire. When DelegationDispatched
    // finally arrives pairing req-B → Worker-A (not Worker-B), the event
    // must be authoritative — NOT blocked by a non-empty task_spec guard.
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_core::lineage::{ExecutorId, ExecutorLineage};

    let mut l = ExecutorLineage::default();

    // Step 1: Worker-A spawns alone.
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-A".into()),
        worktree: std::path::PathBuf::from("/tmp/wA"),
    }));

    // Step 2: DelegationRequested for req-A. If eager-stamp exists, it fires now.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    // Step 3: Worker-B spawns.
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-B".into()),
        worktree: std::path::PathBuf::from("/tmp/wB"),
    }));

    // Step 4: DelegationRequested for req-B.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "coder".into(),
        task: "TASK-B".into(),
        request_id: "req-B".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    // Step 5: Authoritative dispatch — req-B is actually for Worker-A.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("b".into()),
        request_id: "req-B".into(),
        executor_id: "worker-A".into(),
    }));
    // And req-A is for Worker-B.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("b".into()),
        request_id: "req-A".into(),
        executor_id: "worker-B".into(),
    }));

    let a = l.node(&ExecutorId::new("worker-A")).expect("A");
    let b = l.node(&ExecutorId::new("worker-B")).expect("B");
    assert_eq!(
        a.task_spec, "TASK-B",
        "Worker-A owns req-B per authoritative dispatch"
    );
    assert_eq!(
        b.task_spec, "TASK-A",
        "Worker-B owns req-A per authoritative dispatch"
    );
}

// ─── DN-5: INV-1 replay-safety — orphan dispatch + dup-requested warning ────

#[test]
fn dispatched_before_spawned_drains_on_worker_arrival() {
    // Adversarial event order: DelegationRequested → DelegationDispatched
    // both arrive BEFORE the WorkerSpawned for the named executor. The
    // dispatch payload must be buffered and drained when the node finally
    // appears, so task_spec lands on the worker post-spawn.
    let mut l = ExecutorLineage::new();

    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain".into()),
        request_id: "req-A".into(),
        executor_id: "worker-A".into(),
    }));
    // Worker spawns AFTER dispatch — drain must fire here.
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-A".into()),
        worktree: std::path::PathBuf::from("/tmp/wA"),
    }));

    let n = l.node(&ExecutorId::new("worker-A")).expect("worker-A");
    assert_eq!(
        n.task_spec, "TASK-A",
        "orphan-dispatch buffer must drain task onto worker after spawn"
    );
}

#[test]
fn duplicate_delegation_requested_with_same_payload_is_silent() {
    // Identical replay must not panic and must not corrupt buffered payload.
    let mut l = ExecutorLineage::new();

    let ev = SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    });
    l.apply(&ev);
    l.apply(&ev); // identical replay — must stay silent, no panic.
}

#[test]
#[tracing_test::traced_test]
fn duplicate_delegation_requested_with_differing_payload_warns() {
    let mut l = ExecutorLineage::new();

    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain".into()),
        to_agent: "coder".into(),
        task: "TASK-DIFFERENT".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    assert!(logs_contain("duplicate DelegationRequested"));
}
