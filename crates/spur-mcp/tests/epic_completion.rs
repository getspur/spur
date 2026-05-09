use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use spur_acp::domain::{BrainContinuation, ContinuationSource};
use spur_acp::{BrainSessionId, SessionId, SpurEvent, SpurEventBody};
use spur_mcp::plan::audit_sentinel::{
    self, AuditSentinelKind, CompletionState, EpicCompletionOutcome,
};
use spur_mcp::plan::labels;
use spur_mcp::plan::outcomes::{DispatchOutcome, OutcomeStore, SkipReason};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::McpEventSink;
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

const COMPLETION_TASK_TIMEOUT: Duration = Duration::from_secs(60);

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

fn run_br_json(repo: &Path, args: &[&str]) -> String {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
}

fn parse_id_from_create(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("br create json");
    value["id"].as_str().expect("br create id").to_string()
}

fn label_issue(repo: &Path, issue_id: &str, label: &str) {
    run_br(repo, &["label", "add", issue_id, label]);
}

fn continuation_ctx(
    tx: tokio::sync::mpsc::UnboundedSender<BrainContinuation>,
) -> spur_mcp::server::DetachedContinuationCtx {
    spur_mcp::server::DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker_session| {
            let tx = tx.clone();
            Box::pin(async move {
                tx.send(cont).expect("capture continuation");
            })
        }),
    }
}

fn collect_sentinels(list_json: &str) -> Vec<AuditSentinelKind> {
    let items: serde_json::Value = serde_json::from_str(list_json).expect("comments json");
    items
        .as_array()
        .expect("comments array")
        .iter()
        .filter_map(|comment| comment.get("text").and_then(|text| text.as_str()))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(|result| result.ok())
        .collect()
}

struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEvent>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events.lock().unwrap().push(SpurEvent::now(body));
    }
}

async fn beads_pm(repo: &Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn test_dispatch_ctx() -> ReconcilerDispatchCtx {
    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    ReconcilerDispatchCtx {
        delegation_tx,
        task_tracker: tokio_util::task::TaskTracker::new(),
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        event_sink: None,
        materializer: test_materializer(),
        continuation_ctx: common::server_builder::continuation_ctx_arc(),
    }
}

async fn seed_epic_fixture(
    repo: &Path,
    plan_id: &str,
) -> (Arc<spur_pm::PmService>, String, String, String) {
    let epic_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Persisted Epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B",
            "--priority",
            "2",
        ],
    ));

    let plan_label = labels::plan_id(plan_id);
    for issue_id in [&epic_id, &task_a_id, &task_b_id] {
        label_issue(repo, issue_id, &plan_label);
    }
    label_issue(repo, &epic_id, &labels::plan_owner("brain"));
    label_issue(repo, &epic_id, labels::PLAN_COMPLETE);

    (beads_pm(repo).await, epic_id, task_a_id, task_b_id)
}

#[tokio::test]
async fn reconciler_pushes_plan_completed_continuation_after_worker_completion_closes_epic() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let plan_id = "P-reconciler-continuation";
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Reconciler Continuation Epic",
            "--priority",
            "2",
        ],
    ));
    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Reconciler Continuation Task",
            "--priority",
            "2",
        ],
    ));
    let plan_label = labels::plan_id(plan_id);
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &task_id, &plan_label);
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let pm = beads_pm(dir.path()).await;
    let adv = pm.advanced().expect("advanced beads backend");
    for audit in [
        AuditSentinelKind::Dispatch {
            delegation_id: "del-prev".into(),
            worker: "codex".into(),
            attempt: 1,
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-prev".into(),
            completion_state: CompletionState::Failed,
            superseded: false,
            worker_branch: None,
            result_summary: Some("first attempt failed".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::RetryRequested {
            delegation_id: "del-prev".into(),
            attempt: 1,
            error: "first attempt failed".into(),
            worker_branch: None,
            amended_prompt_summary: None,
        },
    ] {
        adv.add_comment(&task_id, &audit_sentinel::encode_comment(&audit))
            .await
            .expect("seed retry history");
    }

    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: Arc::new(continuation_ctx(continuation_tx)),
        }),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );

    assert!(reconciler.tick_once().await.expect("dispatch tick"));
    let request = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
        .await
        .expect("dispatch request should arrive")
        .expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Cancelled {
                reason: "worker cancelled".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("worker cancelled".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send worker result");
    task_tracker.close();
    tokio::time::timeout(COMPLETION_TASK_TIMEOUT, task_tracker.wait())
        .await
        .expect("completion task should finish");

    assert!(reconciler.tick_once().await.expect("epic closure tick"));

    let plan_cont = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let cont = continuation_rx
                .recv()
                .await
                .expect("continuation channel open");
            if cont.source == ContinuationSource::PlanCompleted {
                break cont;
            }
        }
    })
    .await
    .expect("PlanCompleted continuation should fire from reconciler");
    assert_eq!(plan_cont.source, ContinuationSource::PlanCompleted);

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());
}

#[tokio::test]
async fn t_v0d_1_epic_closes_when_children_terminal() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P1").await;

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close child task");
    }
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::REVIEW_REJECTED.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("mark task B terminal failure");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx()),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &epic_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::TerminalWithFailures,
            plan_id,
            epic_id: found_epic_id,
        } if plan_id == "P1" && found_epic_id == &epic_id
    )));
}

#[tokio::test]
async fn t_v0d_2_all_approved_epic_still_yields_plan_ready_to_merge() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P1").await;

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close approved child task");
    }

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: Some(sink_ref),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());
    assert!(
        epic.labels
            .iter()
            .any(|label| label == labels::INTEGRATION_PENDING),
        "all-approved epic must gain integration-pending: {:?}",
        epic.labels
    );

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &epic_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::AllApproved,
            plan_id,
            epic_id: found_epic_id,
        } if plan_id == "P1" && found_epic_id == &epic_id
    )));

    let events = sink.events.lock().unwrap();
    let completed_events = events
        .iter()
        .filter(|event| {
            matches!(
                &event.body,
                SpurEventBody::PlanCompleted { plan_id, .. } if plan_id == "P1"
            )
        })
        .count();
    let ready_events = events
        .iter()
        .filter(|event| {
            matches!(
                &event.body,
                SpurEventBody::PlanReadyToMerge { plan_id } if plan_id == "P1"
            )
        })
        .count();
    assert_eq!(completed_events, 1, "expected one PlanCompleted event");
    assert_eq!(ready_events, 1, "expected one PlanReadyToMerge event");
}

#[tokio::test]
async fn three_task_plan_drops_plan_outcomes_on_epic_close_but_retains_global_ring() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Three Task Epic",
            "--priority",
            "2",
        ],
    ));
    let task_ids = ["A", "B", "C"].map(|suffix| {
        parse_id_from_create(&run_br_json(
            dir.path(),
            &[
                "create",
                "--type",
                "task",
                "--title",
                &format!("Task {suffix}"),
                "--priority",
                "2",
            ],
        ))
    });

    let plan_id = "P-prune";
    let plan_label = labels::plan_id(plan_id);
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    for (index, task_id) in task_ids.iter().enumerate() {
        label_issue(dir.path(), task_id, &plan_label);
        label_issue(
            dir.path(),
            task_id,
            &labels::plan_task_id(&format!("t{index}")),
        );
        label_issue(dir.path(), task_id, &labels::agent("codex"));
    }

    let pm = beads_pm(dir.path()).await;
    let outcomes = Arc::new(tokio::sync::Mutex::new(OutcomeStore::default()));
    {
        let mut store = outcomes.lock().await;
        store.record_no_dispatch_context(None, 3, UNIX_EPOCH);
        store.record_skipped(
            Some(plan_id),
            "phantom-task",
            SkipReason::TaskMissingFromProjection,
            UNIX_EPOCH,
        );
        assert_eq!(store.skip_observations_len(), 1);
    }

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(3);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let mut reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );
    reconciler.set_outcomes(Arc::clone(&outcomes));

    assert!(reconciler.tick_once().await.expect("dispatch tick"));
    let mut requests = Vec::new();
    for _ in 0..3 {
        requests.push(
            tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
                .await
                .expect("dispatch request should arrive")
                .expect("dispatch request"),
        );
    }
    {
        let outcomes = outcomes.lock().await;
        let buffer = outcomes
            .outcomes_by_plan
            .get(plan_id)
            .expect("plan outcome buffer after dispatch");
        assert_eq!(buffer.latest_per_task.len(), 4);
        assert_eq!(outcomes.outcomes_global.snapshot().len(), 1);
        assert_eq!(outcomes.skip_observations_len(), 1);
    }

    for request in requests {
        request
            .respond_to
            .send(spur_acp::DelegationResult {
                status: spur_acp::DelegationStatus::Success,
                diff: None,
                diff_summary: None,
                summary: Some("done".into()),
                estimated_cost_usd: 0.0,
                worker_branch: Some("spur/worker-prune-test".into()),
                artifact: None,
            })
            .expect("send worker result");
    }
    task_tracker.close();
    tokio::time::timeout(COMPLETION_TASK_TIMEOUT, task_tracker.wait())
        .await
        .expect("completion tasks should finish");

    for task_id in &task_ids {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close approved child task");
    }

    assert!(reconciler.tick_once().await.expect("epic closure tick"));
    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());

    let outcomes = outcomes.lock().await;
    assert_eq!(outcomes.outcomes_by_plan.len(), 0);
    assert_eq!(outcomes.skip_observations_len(), 0);
    assert!(matches!(
        outcomes.outcomes_global.snapshot().as_slice(),
        [DispatchOutcome::NoDispatchContext {
            plan_id: None,
            ready_count: 3,
            ..
        }]
    ));
}

#[tokio::test]
async fn epic_completion_backfills_missing_audit_for_closed_terminal_epic() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P1").await;

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close child task");
    }
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::REVIEW_REJECTED.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("mark task B terminal failure");
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close epic without audit");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx()),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &epic_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::TerminalWithFailures,
            plan_id,
            epic_id: found_epic_id,
        } if plan_id == "P1" && found_epic_id == &epic_id
    )));
}

#[tokio::test]
async fn closed_epic_backfill_emits_plan_completed_event() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P2").await;

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close child task");
    }
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::REVIEW_REJECTED.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("mark task B terminal failure");
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close epic without audit");

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: Some(sink_ref),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("P2".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let events = sink.events.lock().unwrap();
    let completed_events = events
        .iter()
        .filter(|event| {
            matches!(
                &event.body,
                SpurEventBody::PlanCompleted {
                    plan_id,
                    approved,
                    rejected,
                    failed,
                    cancelled,
                } if plan_id == "P2"
                    && *approved == 1
                    && *rejected == 1
                    && *failed == 0
                    && *cancelled == 0
            )
        })
        .count();
    assert_eq!(
        completed_events, 1,
        "expected one backfilled PlanCompleted event"
    );
}

#[tokio::test]
async fn closed_epic_backfill_clears_stale_integration_pending_on_failure() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P3").await;

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close child task");
    }
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            add_labels: vec![labels::INTEGRATION_PENDING.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("close epic with stale integration-pending");
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::REVIEW_REJECTED.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("mark task B terminal failure");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx()),
        Some("P3".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label == labels::INTEGRATION_PENDING),
        "closed epic with terminal failures must not keep integration-pending: {:?}",
        epic.labels
    );
}

#[tokio::test]
async fn epic_closure_ignores_non_task_plan_scoped_issues() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_epic_fixture(dir.path(), "P4").await;
    let noise_epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Unrelated Scoped Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &noise_epic_id, &labels::plan_id("P4"));

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close approved child task");
    }

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx()),
        Some("P4".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(
        epic.status,
        pm.closed_status(),
        "non-task issues sharing spur:plan-id must not block epic closure"
    );
}
