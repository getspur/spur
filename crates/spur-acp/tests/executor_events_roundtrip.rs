//! Verifies new executor lineage events round-trip through serde JSON.

use chrono::{TimeZone, Utc};
use spur_acp::{
    AgentKind, BrainInfo, Column, DatasourceEntry, DatasourceKind, IssueSummaryEvent,
    LoopDetailEvent, LoopRunRecordEvent, LoopSummaryEvent, PlanLifecycleEvent,
    PlanLoadWarningEvent, PlanLoopOriginEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent,
    PlanSummaryEvent, ReviewDecision, ReviewKind, ReviewPayload, Role, SessionId, SpurEvent,
    SpurEventBody,
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
    use spur_acp::{ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent};

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
fn prompt_response_roundtrips_usage() {
    use spur_acp::{PromptResponse, StopReason, Usage};

    let response = PromptResponse::new(StopReason::EndTurn).usage(
        Usage::new(123, 45, 78)
            .thought_tokens(9)
            .cached_read_tokens(10)
            .cached_write_tokens(11),
    );

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"usage\""));
    assert!(json.contains("\"totalTokens\":123"));
    assert!(json.contains("\"thoughtTokens\":9"));

    let round: PromptResponse = serde_json::from_str(&json).unwrap();
    let usage = round.usage.expect("usage should round-trip");
    assert_eq!(usage.total_tokens, 123);
    assert_eq!(usage.input_tokens, 45);
    assert_eq!(usage.output_tokens, 78);
    assert_eq!(usage.thought_tokens, Some(9));
    assert_eq!(usage.cached_read_tokens, Some(10));
    assert_eq!(usage.cached_write_tokens, Some(11));
}

#[test]
fn issue_updated_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueUpdated {
        source: "beads".into(),
        id: "BEADS-123".into(),
        status: Some("in_progress".into()),
        assignee: Some("alice".into()),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::IssueUpdated { .. }));
}

#[test]
fn issue_created_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueCreated {
        issue: IssueSummaryEvent {
            id: "BEADS-124".into(),
            source: "beads".into(),
            title: "Add issue-created ACP event".into(),
            status: "open".into(),
            labels: vec!["feature".into()],
            priority: Some(2),
            issue_type: Some("task".into()),
            assignee: Some("alice".into()),
            description: Some("Track one created issue on ACP stream".into()),
        },
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::IssueCreated { .. }));
}

#[test]
fn datasources_changed_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::DatasourcesChanged {
        session: SessionId("brain-data".into()),
        entries: vec![DatasourceEntry {
            name: "sales".into(),
            path: "/tmp/sales.csv".into(),
            kind: DatasourceKind::Csv,
            group: Some("quarterly".into()),
            columns: vec![Column {
                name: "region".into(),
                sql_type: "VARCHAR".into(),
            }],
            row_count: Some(2),
            tables: Vec::new(),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    assert!(matches!(
        round.body,
        SpurEventBody::DatasourcesChanged { .. }
    ));
    assert!(json.contains("DatasourcesChanged"));
    assert!(json.contains("sales"));
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
            epic_id: None,
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
                escalated: 0,
                auto_retried: 0,
            },
            tasks: vec![PlanSnapshotTask {
                task_id: "task-projection".into(),
                task_name: "Build PlanProjection".into(),
                agent: "claude-code".into(),
                issue_id: Some("BEADS-42".into()),
                issue_title: None,
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
            owner_brain_session_id: None,
            owner_token: None,
            owner_acquired_at: None,
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
fn plan_snapshot_deserializes_without_owner_fields_for_backward_compat() {
    // Pre-feature snapshots persisted in NDJSON event logs (~/.kiro/sessions/cli/*.jsonl)
    // omit the owner_* fields entirely. They must continue to deserialize cleanly,
    // with the owner_* fields defaulting to None.
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanSnapshotUpdated": {
            "session_id": "brain-1",
            "snapshot": {
                "plan_id": "plan-pre-feature",
                "status": "running",
                "progress": "0/1 done",
                "next_action": "wait",
                "ready_to_merge": false,
                "counts": {
                    "pending": 1,
                    "ready": 0,
                    "dispatched": 0,
                    "awaiting_review": 0,
                    "approved": 0,
                    "rejected": 0,
                    "failed": 0,
                    "cancelled": 0
                },
                "tasks": []
            }
        }}
    }"#;
    let event: SpurEvent = serde_json::from_str(json)
        .expect("pre-feature PlanSnapshot without owner fields must deserialize");
    let spur_acp::SpurEventBody::PlanSnapshotUpdated { snapshot, .. } = event.body else {
        panic!("expected PlanSnapshotUpdated body");
    };
    assert_eq!(snapshot.plan_id, "plan-pre-feature");
    assert!(snapshot.owner_brain_session_id.is_none());
    assert!(snapshot.owner_token.is_none());
    assert!(snapshot.owner_acquired_at.is_none());
}

#[test]
fn plan_task_failed_roundtrips_from_json() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanTaskFailed": {
            "plan_id": "plan-1",
            "task_id": "task-1",
            "attempt": 2,
            "max_attempts": 3,
            "error": "worker failed",
            "delegation_id": "del-1"
        }}
    }"#;

    let event: SpurEvent = serde_json::from_str(json).expect("PlanTaskFailed must deserialize");
    let encoded = serde_json::to_value(&event).expect("serialize PlanTaskFailed");
    assert_eq!(
        encoded["body"]["PlanTaskFailed"]["plan_id"],
        serde_json::json!("plan-1")
    );
    let _round: SpurEvent = serde_json::from_value(encoded).expect("round-trip PlanTaskFailed");
}

#[test]
fn plan_task_awaiting_review_roundtrips_from_json() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanTaskAwaitingReview": {
            "plan_id": "plan-1",
            "task_id": "task-1",
            "delegation_id": "del-1"
        }}
    }"#;

    let event: SpurEvent =
        serde_json::from_str(json).expect("PlanTaskAwaitingReview must deserialize");
    let encoded = serde_json::to_value(&event).expect("serialize PlanTaskAwaitingReview");
    assert_eq!(
        encoded["body"]["PlanTaskAwaitingReview"]["plan_id"],
        serde_json::json!("plan-1")
    );
    let _round: SpurEvent =
        serde_json::from_value(encoded).expect("round-trip PlanTaskAwaitingReview");
}

#[test]
fn issue_subgraph_loaded_roundtrips() {
    use spur_acp::{GraphEdgeEvent, GraphNodeEvent, SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
        requested_id: "bd-1".into(),
        nodes: vec![GraphNodeEvent {
            id: "bd-1".into(),
            title: Some("Root issue".into()),
            status: Some("open".into()),
            priority: Some(1),
            labels: vec!["epic".into()],
            pagerank: Some(0.9),
        }],
        edges: vec![GraphEdgeEvent {
            from: "bd-1".into(),
            to: "bd-2".into(),
            edge_type: Some("blocks".into()),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::IssueSubgraphLoaded { .. }
    ));
    assert!(json.contains("IssueSubgraphLoaded"));
    assert!(json.contains("Root issue"));
}

#[test]
fn issue_command_error_with_id_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueCommandError {
        operation: "GetIssueGraph".into(),
        error: "bv failed".into(),
        id: Some("bd-root".into()),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::IssueCommandError {
            operation,
            error,
            id,
        } => {
            assert_eq!(operation, "GetIssueGraph");
            assert_eq!(error, "bv failed");
            assert_eq!(id, Some("bd-root".into()));
        }
        other => panic!("expected IssueCommandError, got {other:?}"),
    }
}

#[test]
fn plans_loaded_roundtrips_plan_summary_contract() {
    let ev = SpurEvent::now(SpurEventBody::PlansLoaded {
        plans: vec![
            PlanSummaryEvent {
                plan_id: "plan-a1".into(),
                epic_id: "bd-120".into(),
                title: "Auth migration".into(),
                source_body_preview: Some("Move auth persistence behind the new adapter.".into()),
                owner_state: PlanOwnerStateEvent::Mine,
                lifecycle: PlanLifecycleEvent::Running,
                loop_origin: Some(PlanLoopOriginEvent {
                    loop_id: "loop-daily-triage".into(),
                    generation: 4,
                }),
                counts: Some(PlanSummaryCountsEvent {
                    total: 7,
                    pending: 1,
                    ready: 2,
                    running: 1,
                    awaiting_review: 1,
                    approved: 2,
                    rejected: 0,
                    failed: 0,
                    cancelled: 0,
                }),
                updated_at: Some(Utc.with_ymd_and_hms(2026, 5, 2, 10, 0, 0).unwrap()),
                created_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 9, 30, 0).unwrap()),
            },
            PlanSummaryEvent {
                plan_id: "plan-c3".into(),
                epic_id: "bd-130".into(),
                title: "Owned elsewhere".into(),
                source_body_preview: None,
                owner_state: PlanOwnerStateEvent::Other {
                    owner: "other-brain".into(),
                },
                lifecycle: PlanLifecycleEvent::AwaitingReview,
                loop_origin: None,
                counts: None,
                updated_at: None,
                created_at: None,
            },
            PlanSummaryEvent {
                plan_id: "plan-d4".into(),
                epic_id: "bd-140".into(),
                title: "Ambiguous ownership".into(),
                source_body_preview: None,
                owner_state: PlanOwnerStateEvent::Ambiguous {
                    owners: vec!["brain-a".into(), "brain-b".into()],
                },
                lifecycle: PlanLifecycleEvent::Unknown,
                loop_origin: None,
                counts: None,
                updated_at: None,
                created_at: None,
            },
        ],
        warnings: vec![PlanLoadWarningEvent {
            plan_id: "plan-a1".into(),
            canonical_epic_id: Some("bd-120".into()),
            stale_epic_ids: vec!["bd-stale".into()],
            canonical_owner_state: Some(PlanOwnerStateEvent::Mine),
            message: "Plan plan-a1 has duplicate stale epic bd-stale; using canonical epic bd-120."
                .into(),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::PlansLoaded { plans, warnings } => {
            assert_eq!(plans.len(), 3);
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].plan_id, "plan-a1");
            assert_eq!(warnings[0].canonical_epic_id.as_deref(), Some("bd-120"));
            assert_eq!(warnings[0].stale_epic_ids, vec!["bd-stale"]);
            assert!(matches!(
                warnings[0].canonical_owner_state,
                Some(PlanOwnerStateEvent::Mine)
            ));
            assert!(matches!(plans[0].owner_state, PlanOwnerStateEvent::Mine));
            assert!(matches!(
                plans[1].owner_state,
                PlanOwnerStateEvent::Other { .. }
            ));
            assert!(matches!(
                plans[2].owner_state,
                PlanOwnerStateEvent::Ambiguous { .. }
            ));
            assert_eq!(plans[0].counts.as_ref().unwrap().total, 7);
            assert_eq!(
                plans[0]
                    .loop_origin
                    .as_ref()
                    .map(|origin| { (origin.loop_id.as_str(), origin.generation) }),
                Some(("loop-daily-triage", 4))
            );
            assert!(plans[1].loop_origin.is_none());
            assert!(plans[2].loop_origin.is_none());
        }
        other => panic!("expected PlansLoaded, got {other:?}"),
    }
}

#[test]
fn plans_loaded_deserializes_without_loop_origin_for_backward_compat() {
    let json = serde_json::json!({
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {
            "PlansLoaded": {
                "plans": [{
                    "plan_id": "plan-legacy",
                    "epic_id": "bd-legacy",
                    "title": "Legacy plan summary",
                    "owner_state": "Mine",
                    "lifecycle": "Running"
                }],
                "warnings": []
            }
        }
    });

    let round: SpurEvent = serde_json::from_value(json).unwrap();

    match round.body {
        SpurEventBody::PlansLoaded { plans, warnings } => {
            assert_eq!(plans.len(), 1);
            assert!(warnings.is_empty());
            assert!(plans[0].loop_origin.is_none());
        }
        other => panic!("expected PlansLoaded, got {other:?}"),
    }
}

#[test]
fn plan_command_error_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::PlanCommandError {
        operation: "ResumePlan".into(),
        plan_id: Some("plan-b2".into()),
        error: "resume_plan is not supported by this backend".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::PlanCommandError {
            operation,
            plan_id,
            error,
        } => {
            assert_eq!(operation, "ResumePlan");
            assert_eq!(plan_id, Some("plan-b2".into()));
            assert_eq!(error, "resume_plan is not supported by this backend");
        }
        other => panic!("expected PlanCommandError, got {other:?}"),
    }
}

#[test]
fn agent_config_update_result_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::AgentConfigUpdateResult {
        name: "codex".into(),
        ok: false,
        message: "additional_directories entry is not absolute".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::AgentConfigUpdateResult { name, ok, message } => {
            assert_eq!(name, "codex");
            assert!(!ok);
            assert_eq!(message, "additional_directories entry is not absolute");
        }
        other => panic!("expected AgentConfigUpdateResult, got {other:?}"),
    }
}

#[test]
fn config_update_result_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ConfigUpdateResult {
        section: "graph".into(),
        ok: false,
        message: "unsupported embedding model alias 'not-a-model'".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::ConfigUpdateResult {
            section,
            ok,
            message,
        } => {
            assert_eq!(section, "graph");
            assert!(!ok);
            assert_eq!(message, "unsupported embedding model alias 'not-a-model'");
        }
        other => panic!("expected ConfigUpdateResult, got {other:?}"),
    }
}

#[test]
fn loop_observability_events_roundtrip_with_bounded_payloads() {
    let loops_loaded = SpurEvent::now(SpurEventBody::LoopsLoaded {
        loops: vec![LoopSummaryEvent {
            loop_id: "loop-daily-triage".into(),
            issue_id: "bd-loop".into(),
            title: "Daily triage loop".into(),
            autonomy: Some("l2".into()),
            paused: false,
            retired: false,
            backoff_active: true,
            cadence_secs: 3600,
            effective_interval_secs: 7200,
            next_run: Some(1_783_036_800),
            last_generation: Some(7),
            last_outcome: Some("partial".into()),
            last_cost_micros: Some(42_000),
            consecutive_failures: 2,
            goal_preview: Some("Keep the issue queue under control.".into()),
            updated_at: Some(Utc.with_ymd_and_hms(2026, 5, 3, 10, 15, 0).unwrap()),
        }],
        warnings: vec!["Loop list truncated at 200 rows.".into()],
    });

    let encoded = serde_json::to_value(&loops_loaded).unwrap();
    assert_eq!(
        encoded["body"]["LoopsLoaded"]["loops"][0]["effective_interval_secs"],
        serde_json::json!(7200)
    );
    assert_eq!(
        encoded["body"]["LoopsLoaded"]["loops"][0]["goal_preview"],
        serde_json::json!("Keep the issue queue under control.")
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, loops_loaded.body);

    let detail_loaded = SpurEvent::now(SpurEventBody::LoopDetailLoaded {
        detail: LoopDetailEvent {
            loop_id: "loop-daily-triage".into(),
            issue_id: "bd-loop".into(),
            title: "Daily triage loop".into(),
            goal_preview: Some("Keep the issue queue under control.".into()),
            cadence_secs: 3600,
            effective_interval_secs: 7200,
            backoff_active: true,
            paused: false,
            next_run: Some(1_783_036_800),
            consecutive_failures: 2,
            budget_micros_per_generation: Some(100_000),
            max_generations_per_day: Some(8),
            max_tasks: Some(12),
            recent_runs: vec![LoopRunRecordEvent {
                generation: 7,
                outcome: "partial".into(),
                cost_micros: 42_000,
                autonomy: Some("l2".into()),
            }],
        },
    });

    let encoded = serde_json::to_value(&detail_loaded).unwrap();
    assert_eq!(
        encoded["body"]["LoopDetailLoaded"]["detail"]["recent_runs"][0]["generation"],
        serde_json::json!(7)
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, detail_loaded.body);

    let command_error_with_loop = SpurEvent::now(SpurEventBody::LoopCommandError {
        operation: "PauseLoop".into(),
        loop_id: Some("loop-daily-triage".into()),
        error: "loop issue not found".into(),
    });
    let encoded = serde_json::to_value(&command_error_with_loop).unwrap();
    assert_eq!(
        encoded["body"]["LoopCommandError"]["loop_id"],
        serde_json::json!("loop-daily-triage")
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, command_error_with_loop.body);

    let command_error_without_loop = SpurEvent::now(SpurEventBody::LoopCommandError {
        operation: "RefreshLoops".into(),
        loop_id: None,
        error: "backend unavailable".into(),
    });
    let encoded = serde_json::to_value(&command_error_without_loop).unwrap();
    let payload = encoded["body"]["LoopCommandError"]
        .as_object()
        .expect("loop command error payload");
    assert!(!payload.contains_key("loop_id"));
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, command_error_without_loop.body);
}

#[test]
fn loop_events_roundtrip_with_bounded_payloads() {
    let cases = [
        SpurEventBody::LoopArmed {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            next_run: 1_783_036_800,
        },
        SpurEventBody::LoopGenerationStarted {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            plan_id: "plan-7".into(),
        },
        SpurEventBody::LoopRunRecorded {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            outcome: "partial".into(),
            cost_micros: 42_000,
        },
        SpurEventBody::LoopPaused {
            loop_id: "loop-daily-triage".into(),
            by: "auto_paused".into(),
        },
    ];

    for body in cases {
        let event = SpurEvent::now(body);
        let encoded = serde_json::to_value(&event).expect("serialize loop event");
        let round: SpurEvent =
            serde_json::from_value(encoded.clone()).expect("round-trip loop event");
        assert_eq!(round.body, event.body);

        let payload = encoded["body"].as_object().expect("body object");
        let payload = payload.values().next().expect("loop event payload");
        assert_eq!(payload["loop_id"], serde_json::json!("loop-daily-triage"));
    }
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

#[test]
fn dispatch_lease_expired_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::DispatchLeaseExpired {
        plan_id: "p1".into(),
        task_id: "t1".into(),
        issue_id: "bd-1".into(),
        delegation_id: "del-A".into(),
        expired_at: 1_777_777_777,
        age_secs: 42,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::DispatchLeaseExpired { .. }
    ));
    assert!(json.contains("DispatchLeaseExpired"));
}

#[test]
fn plan_task_blocked_on_setup_conflict_roundtrips() {
    use spur_acp::domain::continuation::SetupConflictTopology;
    use spur_acp::domain::events::DiffSummary;
    use spur_acp::{SpurEvent, SpurEventBody};

    let topology = SetupConflictTopology {
        base_oid: "2779409d".into(),
        blocked_task_id: "task-9".into(),
        conflict_dep_task_id: "task-7".into(),
        conflict_files: vec!["src/main.rs".into()],
        approved_chain: vec![spur_acp::domain::continuation::ApprovedTaskGitNode {
            task_id: "task-5".into(),
            worker_branch: "spur/worker/v2/codex/owner/task-5".into(),
            tip_oid: "b786d770".into(),
            parent_oid: "2779409d".into(),
            cumulative_diff_stat: DiffSummary {
                files_changed: 2,
                insertions: 10,
                deletions: 3,
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            incremental_diff_stat: DiffSummary {
                files_changed: 2,
                insertions: 10,
                deletions: 3,
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            appears_flattened: false,
        }],
    };

    let ev = SpurEvent::now(SpurEventBody::PlanTaskBlockedOnSetupConflict {
        plan_id: "plan-9".into(),
        task_id: "task-9".into(),
        delegation_id: "del-9".into(),
        dep_task_id: "task-7".into(),
        files: vec!["src/main.rs".into()],
        topology: Some(topology),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::PlanTaskBlockedOnSetupConflict {
            plan_id,
            task_id,
            delegation_id,
            dep_task_id,
            files,
            topology,
        } => {
            assert_eq!(plan_id, "plan-9");
            assert_eq!(task_id, "task-9");
            assert_eq!(delegation_id, "del-9");
            assert_eq!(dep_task_id, "task-7");
            assert_eq!(files, vec!["src/main.rs"]);
            assert!(topology.is_some());
            let topo = topology.unwrap();
            assert_eq!(topo.base_oid, "2779409d");
            assert_eq!(topo.approved_chain.len(), 1);
            assert_eq!(topo.approved_chain[0].task_id, "task-5");
        }
        other => panic!("expected PlanTaskBlockedOnSetupConflict, got {other:?}"),
    }
    assert!(json.contains("PlanTaskBlockedOnSetupConflict"));
    assert!(json.contains("topology"));
}

#[test]
fn worker_session_configured_roundtrips() {
    use spur_acp::domain::delegation::ResolvedSessionConfig;
    use std::collections::BTreeMap;

    let mut config_overrides_applied = BTreeMap::new();
    config_overrides_applied.insert("mode".to_string(), "plan".to_string());

    let ev = SpurEvent::now(SpurEventBody::WorkerSessionConfigured {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        config: ResolvedSessionConfig {
            agent: "codex".into(),
            profile: Some("rust-pro".into()),
            model: Some("gpt-5-codex".into()),
            effort: Some("high".into()),
            config_overrides_applied,
            skipped: vec![
                "effort: agent exposed no thought-level option (requested 'high')".into(),
            ],
            outcome_warning: Some("worktree normalized".into()),
        },
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::WorkerSessionConfigured {
            brain_session_id,
            executor_id,
            config,
        } => {
            assert_eq!(brain_session_id, SessionId("brain-1".into()));
            assert_eq!(executor_id, "exec-1");
            assert_eq!(config.agent, "codex");
            assert_eq!(config.profile.as_deref(), Some("rust-pro"));
            assert_eq!(config.model.as_deref(), Some("gpt-5-codex"));
            assert_eq!(config.effort.as_deref(), Some("high"));
            assert_eq!(
                config
                    .config_overrides_applied
                    .get("mode")
                    .map(String::as_str),
                Some("plan")
            );
            assert_eq!(config.skipped.len(), 1);
            assert_eq!(
                config.outcome_warning.as_deref(),
                Some("worktree normalized")
            );
        }
        other => panic!("expected WorkerSessionConfigured, got {other:?}"),
    }
    assert!(json.contains("WorkerSessionConfigured"));
    assert!(json.contains("gpt-5-codex"));
}

#[test]
fn worker_session_configured_defaults_are_compact_when_absent() {
    // A resolved config with no profile/model/effort/overrides/skips should
    // serialize its Option/collection fields away entirely (they're all
    // `#[serde(default, skip_serializing_if = ...)]`), keeping the wire
    // payload small for the common "nothing overridden" case.
    use spur_acp::domain::delegation::ResolvedSessionConfig;

    let ev = SpurEvent::now(SpurEventBody::WorkerSessionConfigured {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        config: ResolvedSessionConfig {
            agent: "claude-code".into(),
            ..Default::default()
        },
    });

    let value = serde_json::to_value(&ev).unwrap();
    let config = &value["body"]["WorkerSessionConfigured"]["config"];
    assert_eq!(config["agent"], serde_json::json!("claude-code"));
    let obj = config.as_object().expect("config object");
    assert!(!obj.contains_key("profile"));
    assert!(!obj.contains_key("model"));
    assert!(!obj.contains_key("effort"));
    assert!(!obj.contains_key("config_overrides_applied"));
    assert!(!obj.contains_key("skipped"));
    assert!(!obj.contains_key("outcome_warning"));

    let _round: SpurEvent = serde_json::from_value(value).expect("round-trip");
}

#[test]
fn delegation_result_deserializes_without_resolved_config_for_backward_compat() {
    // Pre-existing outcome artifacts persisted before `resolved_config`
    // existed must still deserialize cleanly, with the field defaulting to
    // `None` — this is the additive/optional backward-compat contract for
    // `fetch_outcome_artifact` consumers reading older blobs.
    use spur_acp::domain::DelegationResult;

    let json = serde_json::json!({
        "status": "Success",
        "diff": null,
        "summary": "done",
        "estimated_cost_usd": 0.01,
        "worker_branch": "spur/worker-x",
    });

    let result: DelegationResult =
        serde_json::from_value(json).expect("pre-existing DelegationResult must deserialize");
    assert!(result.resolved_config.is_none());
}

#[test]
fn brain_switched_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitched {
        from: "grok".into(),
        to: "opencode".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainSwitched { from, to } => {
            assert_eq!(from, "grok");
            assert_eq!(to, "opencode");
        }
        other => panic!("expected BrainSwitched, got {other:?}"),
    }
}

#[test]
fn brain_switch_noop_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitchNoop {
        name: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::BrainSwitchNoop { name } if name == "grok"
    ));
}

#[test]
fn brain_switch_error_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitchError {
        name: "nope".into(),
        available: vec!["grok".into(), "opencode".into()],
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainSwitchError { name, available } => {
            assert_eq!(name, "nope");
            assert_eq!(available, vec!["grok", "opencode"]);
        }
        other => panic!("expected BrainSwitchError, got {other:?}"),
    }
}

#[test]
fn brains_listed_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainsListed {
        brains: vec![BrainInfo {
            name: "grok".into(),
            kind: AgentKind::Grok,
            is_default: true,
        }],
        active: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainsListed { brains, active } => {
            assert_eq!(active, "grok");
            assert_eq!(brains.len(), 1);
            assert_eq!(brains[0].name, "grok");
            assert_eq!(brains[0].kind, AgentKind::Grok);
            assert!(brains[0].is_default);
        }
        other => panic!("expected BrainsListed, got {other:?}"),
    }
}

#[test]
fn brain_picker_open_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainPickerOpen {
        brains: vec![BrainInfo {
            name: "opencode".into(),
            kind: AgentKind::OpenCode,
            is_default: false,
        }],
        active: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainPickerOpen { brains, active } => {
            assert_eq!(active, "grok");
            assert_eq!(brains[0].name, "opencode");
            assert_eq!(brains[0].kind, AgentKind::OpenCode);
        }
        other => panic!("expected BrainPickerOpen, got {other:?}"),
    }
}

#[test]
fn brain_retired_brain_switch_reason_roundtrips() {
    use spur_acp::domain::events::BrainRetireReason;
    let ev = SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("s1".into()),
        reason: BrainRetireReason::BrainSwitch,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainRetired { reason, .. } => {
            assert_eq!(reason, BrainRetireReason::BrainSwitch);
        }
        other => panic!("expected BrainRetired, got {other:?}"),
    }
}
