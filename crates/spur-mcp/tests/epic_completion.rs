use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId, SpurEvent, SpurEventBody};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, EpicCompletionOutcome};
use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::McpEventSink;
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let output = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        );
    }
}

fn run_br_json(repo: &Path, args: &[&str]) -> String {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--json");
    let output = Command::new("br")
        .args(&full_args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        );
    }
}

fn parse_id_from_create(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("br create json");
    value["id"].as_str().expect("br create id").to_string()
}

fn label_issue(repo: &Path, issue_id: &str, label: &str) {
    run_br(repo, &["label", "add", issue_id, label]);
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
    label_issue(repo, &epic_id, labels::PLAN_COMPLETE);

    (beads_pm(repo).await, epic_id, task_a_id, task_b_id)
}

#[tokio::test]
async fn t_v0d_1_epic_closes_when_children_terminal() {
    if !br_available() {
        eprintln!("skipping t_v0d_1_epic_closes_when_children_terminal: `br` not on PATH");
        return;
    }

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
        None,
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
    if !br_available() {
        eprintln!(
            "skipping t_v0d_2_all_approved_epic_still_yields_plan_ready_to_merge: `br` not on PATH"
        );
        return;
    }

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
async fn epic_completion_backfills_missing_audit_for_closed_terminal_epic() {
    if !br_available() {
        eprintln!(
            "skipping epic_completion_backfills_missing_audit_for_closed_terminal_epic: `br` not on PATH"
        );
        return;
    }

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
        None,
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
    if !br_available() {
        eprintln!("skipping closed_epic_backfill_emits_plan_completed_event: `br` not on PATH");
        return;
    }

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
    if !br_available() {
        eprintln!(
            "skipping closed_epic_backfill_clears_stale_integration_pending_on_failure: `br` not on PATH"
        );
        return;
    }

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
        None,
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
    if !br_available() {
        eprintln!("skipping epic_closure_ignores_non_task_plan_scoped_issues: `br` not on PATH");
        return;
    }

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
        None,
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
