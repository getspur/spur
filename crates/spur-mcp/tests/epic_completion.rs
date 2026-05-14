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
use spur_mcp::plan::PmLike;
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

async fn mock_issue(
    pm: &spur_mcp::plan::test_util::MockPm,
    title: &str,
    body: &str,
    issue_type: &str,
    labels: Vec<String>,
    depends_on: Vec<String>,
) -> String {
    pm.create_issue(spur_pm::IssueCreate {
        title: title.to_string(),
        description: Some(body.to_string()),
        issue_type: Some(issue_type.to_string()),
        priority: Some(2),
        labels,
        parent: None,
        depends_on,
        ..Default::default()
    })
    .await
    .expect("create mock issue")
}

async fn seed_mock_plan(
    pm: &spur_mcp::plan::test_util::MockPm,
    plan_id: &str,
    tasks: &[(&str, &[&str])],
) -> (String, std::collections::HashMap<String, String>) {
    let epic_id = mock_issue(
        pm,
        "Mock Persisted Epic",
        "Mock persisted epic",
        "epic",
        vec![
            labels::plan_id(plan_id),
            labels::plan_owner("brain"),
            labels::PLAN_COMPLETE.to_string(),
        ],
        vec![],
    )
    .await;
    let mut by_task = std::collections::HashMap::new();
    for (task_id, deps) in tasks {
        let depends_on = deps
            .iter()
            .map(|dep| {
                by_task
                    .get(*dep)
                    .cloned()
                    .unwrap_or_else(|| panic!("dependency {dep} must be created first"))
            })
            .collect();
        let issue_id = mock_issue(
            pm,
            &format!("Task {task_id}"),
            &format!("Do task {task_id}"),
            "task",
            vec![
                labels::plan_id(plan_id),
                labels::plan_task_id(task_id),
                labels::agent("codex"),
            ],
            depends_on,
        )
        .await;
        by_task.insert((*task_id).to_string(), issue_id);
    }
    (epic_id, by_task)
}

fn mock_dispatch_ctx(
    event_sink: Option<Arc<dyn McpEventSink>>,
    continuation_ctx: Arc<spur_mcp::server::DetachedContinuationCtx>,
) -> (
    ReconcilerDispatchCtx,
    tokio::sync::mpsc::Receiver<spur_mcp::DelegationRequest>,
    tokio_util::task::TaskTracker,
) {
    let (delegation_tx, delegation_rx) = tokio::sync::mpsc::channel(4);
    let task_tracker = tokio_util::task::TaskTracker::new();
    (
        ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink,
            materializer: test_materializer(),
            continuation_ctx,
        },
        delegation_rx,
        task_tracker,
    )
}

fn mock_reconciler(
    pm: Arc<spur_mcp::plan::test_util::MockPm>,
    plan_id: &str,
    dispatch: Option<ReconcilerDispatchCtx>,
) -> Reconciler {
    let pm_like: Arc<dyn PmLike> = pm;
    Reconciler::new_with_pm_like(
        ReconcilerConfig::default(),
        pm_like,
        Arc::new(Notify::new()),
        dispatch,
        Some(plan_id.to_string()),
        common::server_builder::pro_feature_gate(),
    )
}

async fn wait_for_mock_issue_status(
    pm: &spur_mcp::plan::test_util::MockPm,
    issue_id: &str,
    expected: &str,
) {
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if pm.issue(issue_id).await.status == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let issue = pm.issue(issue_id).await;
    panic!(
        "timed out waiting for mock issue {issue_id} status {expected}, got {}",
        issue.status
    );
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

    let pm = beads_pm(repo).await;
    let adv = pm.advanced().expect("advanced beads backend");
    for (task_id, delegation_id, worker_branch) in [
        (task_a_id.as_str(), "del-a", "spur/worker-a"),
        (task_b_id.as_str(), "del-b", "spur/worker-b"),
    ] {
        adv.add_comment(
            task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: delegation_id.to_string(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some(worker_branch.to_string()),
                result_summary: Some("fixture completion".to_string()),
                artifact_uri: None,
                dispatched_base_oid: Some("0000000000000000000000000000000000000001".to_string()),
            }),
        )
        .await
        .expect("seed completion");
        adv.add_comment(
            task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
                delegation_id: delegation_id.to_string(),
            }),
        )
        .await
        .expect("seed approval");
    }

    (pm, epic_id, task_a_id, task_b_id)
}

#[tokio::test]
async fn mock_pm_reconciler_cancelled_task_does_not_cascade_fail_dependent() {
    let pm = spur_mcp::plan::test_util::MockPm::new().arc();
    let plan_id = "P-mock-cancel-dn4";
    let (_epic_id, task_issues) = seed_mock_plan(&pm, plan_id, &[("A", &[]), ("B", &["A"])]).await;
    let task_a_issue = task_issues["A"].clone();
    let task_b_issue = task_issues["B"].clone();

    let (dispatch, mut delegation_rx, task_tracker) =
        mock_dispatch_ctx(None, common::server_builder::continuation_ctx_arc());
    let reconciler = mock_reconciler(Arc::clone(&pm), plan_id, Some(dispatch));

    assert!(reconciler.tick_once().await.expect("dispatch A"));
    let request = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
        .await
        .expect("A dispatch should arrive")
        .expect("A dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_a_issue.as_str()));
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Cancelled {
                reason: "brain cancelled root".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("brain cancelled root".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send cancellation");
    wait_for_mock_issue_status(&pm, &task_a_issue, pm.closed_status()).await;

    assert!(reconciler
        .tick_once()
        .await
        .expect("dispatch B after A cancelled"));
    let dependent = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
        .await
        .expect("B dispatch should arrive")
        .expect("B dispatch request");
    assert_eq!(dependent.issue_id.as_deref(), Some(task_b_issue.as_str()));
    assert_eq!(
        pm.issue(&task_b_issue).await.status,
        "open",
        "dependent must remain dispatchable, not cascade-failed"
    );
    dependent
        .respond_to
        .send(spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Cancelled {
                reason: "finish test".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("finish test".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send dependent cancellation");
    wait_for_mock_issue_status(&pm, &task_b_issue, pm.closed_status()).await;
    task_tracker.close();
    tokio::time::timeout(COMPLETION_TASK_TIMEOUT, task_tracker.wait())
        .await
        .expect("mock completion tasks should finish");
}

#[tokio::test]
async fn mock_pm_reconciler_plan_completed_counts_cancelled_and_suppresses_ready_to_merge() {
    let pm = spur_mcp::plan::test_util::MockPm::new().arc();
    let plan_id = "P-mock-cancelled-complete";
    let (epic_id, task_issues) = seed_mock_plan(&pm, plan_id, &[("A", &[]), ("B", &[])]).await;
    let task_a_issue = task_issues["A"].clone();
    let task_b_issue = task_issues["B"].clone();
    let adv = pm.advanced().expect("mock advanced PM");
    adv.add_comment(
        &task_a_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
            delegation_id: "del-A".into(),
            worker: "codex".into(),
            attempt: 1,
        }),
    )
    .await
    .expect("seed dispatch A");
    adv.add_comment(
        &task_a_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-A".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-A".into()),
            result_summary: Some("A ready".into()),
            artifact_uri: None,
            dispatched_base_oid: Some("0000000000000000000000000000000000000001".into()),
        }),
    )
    .await
    .expect("seed completion A");
    adv.add_comment(
        &task_a_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
            delegation_id: "del-A".into(),
        }),
    )
    .await
    .expect("seed approval A");
    pm.update_issue(
        &task_a_issue,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close approved A");

    adv.add_comment(
        &task_b_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
            delegation_id: "del-B".into(),
            worker: "codex".into(),
            attempt: 1,
        }),
    )
    .await
    .expect("seed dispatch B");
    adv.add_comment(
        &task_b_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-B".into(),
            completion_state: CompletionState::Cancelled,
            superseded: false,
            worker_branch: None,
            result_summary: Some("B cancelled".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .expect("seed cancellation B");
    pm.update_issue(
        &task_b_issue,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close cancelled B");

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (dispatch, _delegation_rx, _task_tracker) =
        mock_dispatch_ctx(Some(sink_ref), Arc::new(continuation_ctx(continuation_tx)));
    let reconciler = mock_reconciler(Arc::clone(&pm), plan_id, Some(dispatch));

    assert!(reconciler.tick_once().await.expect("close mixed epic"));
    let epic = pm.issue(&epic_id).await;
    assert_eq!(epic.status, pm.closed_status());
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label == labels::INTEGRATION_PENDING),
        "cancelled plans must not be ready to merge: {:?}",
        epic.labels
    );

    let events = sink.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.body,
                SpurEventBody::PlanCompleted {
                    plan_id: found,
                    approved: 1,
                    rejected: 0,
                    failed: 0,
                    cancelled: 1,
                } if found == plan_id
            ))
            .count(),
        1,
        "expected PlanCompleted with cancelled count"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.body,
                SpurEventBody::PlanReadyToMerge { plan_id: found } if found == plan_id
            ))
            .count(),
        0,
        "cancelled count must suppress PlanReadyToMerge"
    );
    drop(events);

    let continuation = tokio::time::timeout(Duration::from_secs(2), continuation_rx.recv())
        .await
        .expect("PlanCompleted continuation should arrive")
        .expect("continuation channel open");
    assert_eq!(continuation.source, ContinuationSource::PlanCompleted);
}

#[tokio::test]
#[ignore = "TODO bd-d1r-fu-mock-reconciler: dispatched_base_oid not threaded through MockPm dispatch→completion audit path"]
async fn mock_pm_reconciler_success_completion_fires_awaiting_review_continuation() {
    let pm = spur_mcp::plan::test_util::MockPm::new().arc();
    let plan_id = "P-mock-awaiting-review-continuation";
    let (_epic_id, task_issues) = seed_mock_plan(&pm, plan_id, &[("A", &[])]).await;
    let task_issue = task_issues["A"].clone();
    pm.update_issue(
        &task_issue,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::dispatched_base_oid(
                "0000000000000000000000000000000000000001",
            )],
            ..Default::default()
        },
    )
    .await
    .expect("seed dispatched base oid label");
    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (dispatch, mut delegation_rx, task_tracker) =
        mock_dispatch_ctx(None, Arc::new(continuation_ctx(continuation_tx)));
    let reconciler = mock_reconciler(Arc::clone(&pm), plan_id, Some(dispatch));

    assert!(reconciler.tick_once().await.expect("dispatch A"));
    let request = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
        .await
        .expect("dispatch should arrive")
        .expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_issue.as_str()));
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("A awaits review".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-A".into()),
            artifact: None,
        })
        .expect("send success");

    let continuation = tokio::time::timeout(Duration::from_secs(2), continuation_rx.recv())
        .await
        .expect("awaiting-review continuation should arrive")
        .expect("continuation channel open");
    assert_eq!(
        continuation.source,
        ContinuationSource::PlanTaskAwaitingReview
    );
    wait_for_mock_issue_status(&pm, &task_issue, "open").await;
    assert!(pm
        .issue(&task_issue)
        .await
        .labels
        .iter()
        .any(|label| label == labels::READY_FOR_REVIEW));
    task_tracker.close();
    tokio::time::timeout(COMPLETION_TASK_TIMEOUT, task_tracker.wait())
        .await
        .expect("mock completion tasks should finish");
}

#[tokio::test]
async fn mock_pm_reconciler_terminal_failure_fires_escalated_task_continuation() {
    let pm = spur_mcp::plan::test_util::MockPm::new().arc();
    let plan_id = "P-mock-failed-continuation";
    let (_epic_id, task_issues) = seed_mock_plan(&pm, plan_id, &[("A", &[])]).await;
    let task_issue = task_issues["A"].clone();
    let adv = pm.advanced().expect("mock advanced PM");
    for attempt in 1..=2 {
        let delegation_id = format!("del-prev-{attempt}");
        adv.add_comment(
            &task_issue,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
                delegation_id: delegation_id.clone(),
                worker: "codex".into(),
                attempt,
            }),
        )
        .await
        .expect("seed previous dispatch");
        adv.add_comment(
            &task_issue,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: delegation_id.clone(),
                completion_state: CompletionState::Failed,
                superseded: false,
                worker_branch: None,
                result_summary: Some("previous failure".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            }),
        )
        .await
        .expect("seed previous failure");
        adv.add_comment(
            &task_issue,
            &audit_sentinel::encode_comment(&AuditSentinelKind::RetryRequested {
                delegation_id,
                attempt,
                error: "previous failure".into(),
                worker_branch: None,
                amended_prompt_summary: None,
            }),
        )
        .await
        .expect("seed previous retry");
    }

    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (dispatch, mut delegation_rx, task_tracker) =
        mock_dispatch_ctx(None, Arc::new(continuation_ctx(continuation_tx)));
    let reconciler = mock_reconciler(Arc::clone(&pm), plan_id, Some(dispatch));

    assert!(reconciler
        .tick_once()
        .await
        .expect("dispatch final attempt"));
    let request = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv())
        .await
        .expect("dispatch should arrive")
        .expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_issue.as_str()));
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            status: spur_acp::DelegationStatus::Failed {
                error: "final failure".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("final failure".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send final failure");

    let continuation = tokio::time::timeout(Duration::from_secs(2), continuation_rx.recv())
        .await
        .expect("escalated-task continuation should arrive")
        .expect("continuation channel open");
    assert_eq!(continuation.source, ContinuationSource::PlanTaskEscalated);
    wait_for_mock_issue_status(&pm, &task_issue, "open").await;
    assert!(pm
        .issue(&task_issue)
        .await
        .labels
        .iter()
        .any(|label| label == spur_mcp::plan::mutation_executor::SIGNAL_ESCALATED_LABEL));
    task_tracker.close();
    tokio::time::timeout(COMPLETION_TASK_TIMEOUT, task_tracker.wait())
        .await
        .expect("mock completion tasks should finish");
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
#[ignore = "TODO bd-d1r-fu-mock-reconciler: dispatched_base_oid not threaded through MockPm dispatch→completion audit path"]
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
        label_issue(
            dir.path(),
            task_id,
            &labels::dispatched_base_oid("0000000000000000000000000000000000000001"),
        );
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
