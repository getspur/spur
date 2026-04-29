//! Verifies new executor lineage events round-trip through serde JSON.

use spur_acp::{
    ReviewDecision, ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody,
};

#[test]
fn executor_phase_changed_rejects_invalid_variant() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorPhaseChanged": {"id": "x", "phase": "running"}}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "lowercase 'running' must fail to deserialize"
    );
}

#[test]
fn executor_spawned_rejects_invalid_role() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorSpawned": {
            "id": "x", "parent_id": null,
            "session_id": "s",
            "agent": "a", "role": "brain", "task_spec": ""
        }}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "lowercase 'brain' must fail to deserialize"
    );
}

#[test]
fn executor_spawned_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: Some("brain-1".into()),
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "fix bug".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::ExecutorSpawned { .. }));
}

#[test]
fn executor_review_resolved_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "exec-1".into(),
        decision: ReviewDecision::Reject {
            reason: "tests fail".into(),
        },
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::ExecutorReviewResolved { .. }
    ));
}

#[test]
fn executor_review_requested_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
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
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::ExecutorReviewRequested { .. }
    ));
}

#[test]
fn executor_review_requested_carries_attempt_n() {
    use spur_acp::{ReviewKind, ReviewPayload, SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 2,
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
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_value(&event).unwrap();
    assert_eq!(j["body"]["ExecutorReviewRequested"]["attempt_n"], 2);
    let _back: SpurEvent = serde_json::from_value(j).expect("round-trip");
}

#[test]
fn executor_review_cancelled_round_trips() {
    use spur_acp::{SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewCancelled {
        id: "exec-1".into(),
        reason: "brain call cancelled".into(),
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_string(&event).expect("serialize");
    let _back: SpurEvent = serde_json::from_str(&j).expect("round-trip");
    assert!(j.contains("ExecutorReviewCancelled"));
    assert!(j.contains("brain call cancelled"));
}

#[test]
fn worker_notification_roundtrips() {
    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
    };
    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("thinking...")));
    let notification =
        SessionNotification::new("acp-sess", SessionUpdate::AgentThoughtChunk(chunk));
    let ev = SpurEvent::now(SpurEventBody::WorkerNotification {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        notification: Box::new(notification),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::WorkerNotification { .. }
    ));
    assert!(json.contains("WorkerNotification"));
    assert!(json.contains("thinking..."));
}

#[test]
fn plan_snapshot_updated_roundtrips() {
    use spur_acp::{
        PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody,
    };

    let ev = SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "p-123".into(),
            status: "running".into(),
            progress: "1/3 reviewed, 1 running, 1 pending".into(),
            next_action: "Workers still running. Poll get_plan_status to monitor.".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 1,
                ready: 0,
                dispatched: 1,
                awaiting_review: 1,
                approved: 0,
                rejected: 0,
                failed: 0,
                cancelled: 0,
            },
            tasks: vec![PlanSnapshotTask {
                task_id: "task-projection".into(),
                task_name: "Build PlanProjection".into(),
                agent: "claude-code".into(),
                issue_id: Some("BEADS-42".into()),
                status: "awaiting_review".into(),
                attempt: 1,
                max_attempts: 3,
                depends_on: vec!["task-contract".into()],
                blocked_by: Vec::new(),
                unblocks: vec!["task-app".into()],
                summary: Some("projects plan status into UI".into()),
                feedback: None,
                error: None,
                worker_branch: Some("spur/worker-123".into()),
                delegation_id: Some("del-123".into()),
                diff_summary: None,
                mutation_id: None,
                superseded_by: Vec::new(),
                next_action: "review".into(),
            }],
        }),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::PlanSnapshotUpdated { .. }
    ));
}

#[test]
fn plan_snapshot_updated_rejects_malformed_payload() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanSnapshotUpdated": {
            "session_id": "brain-1",
            "snapshot": {
                "status": "running",
                "progress": "1/3 reviewed, 1 running, 1 pending",
                "next_action": "review",
                "ready_to_merge": false,
                "counts": {
                    "pending": 1,
                    "ready": 0,
                    "dispatched": 1,
                    "awaiting_review": 1,
                    "approved": 0,
                    "rejected": 0,
                    "failed": 0,
                    "cancelled": 0
                },
                "tasks": []
            }
        }}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "missing required plan_id must fail to deserialize"
    );
}

#[test]
fn plan_completed_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanCompleted {
        plan_id: "p1".into(),
        approved: 3,
        rejected: 1,
        failed: 0,
        cancelled: 0,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanCompleted { .. }));
}

#[test]
fn plan_ready_to_merge_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanReadyToMerge {
        plan_id: "p1".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanReadyToMerge { .. }));
}

#[test]
fn plan_pending_sweep_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanPendingSweep {
        plan_id: Some("p1".into()),
        epic_id: "bd-epic".into(),
        action: "quarantined".into(),
        child_count: 2,
        age_secs: 3601,
        reason: "stale pending plan exceeded grace".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanPendingSweep { .. }));
    assert!(json.contains("PlanPendingSweep"));
    assert!(json.contains("quarantined"));
}
