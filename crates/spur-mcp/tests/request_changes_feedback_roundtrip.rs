//! bd-33it end-to-end: request_changes feedback must survive projector
//! reprojection and reach the worker in the re-dispatched enriched task.

use std::path::Path;
use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::plan::{PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};

mod common;

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    common::beads::run_br(repo, args).map(|_| ())
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

async fn add_labels(pm: &PmService, issue_id: &str, labels_to_add: &[String]) {
    for label in labels_to_add {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .expect("add label");
    }
}

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

#[tokio::test]
async fn request_changes_feedback_survives_reprojection_and_reaches_worker() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-bd-33it",
        "Plan bd-33it",
        None,
        &[PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Implement the feature".into(),
            depends_on: vec![],
            issue_id: None,
            context_files: vec![],
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph.task_map.get("t1").expect("task id").clone();
    add_labels(
        pm.as_ref(),
        &subgraph.epic_id,
        &[spur_mcp::plan::labels::plan_owner("brain")],
    )
    .await;

    add_labels(
        pm.as_ref(),
        &task_issue_id,
        &[spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()],
    )
    .await;

    let state = Arc::new(Mutex::new(PlanState {
        plan_id: "plan-bd-33it".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Implement the feature".into(),
                depends_on: Vec::new(),
                issue_id: Some(task_issue_id.clone()),
                context_files: Vec::new(),
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("done".into()),
            },
            result: None,
            worker_branch: Some("feat/task".into()),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: Some("del-A".into()),
            dispatched_base_oid: None,
        }],
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: Some(subgraph.epic_id.clone()),
    }));

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();
    let _ = spur_mcp::plan::handle_review_task(
        state,
        "plan-bd-33it",
        "t1",
        "request_changes",
        Some("fix the edge case with null inputs"),
        Some(pm_arc),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("request_changes");

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-bd-33it".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");
    let request = tokio::time::timeout(std::time::Duration::from_secs(10), delegation_rx.recv())
        .await
        .expect("redispatch request within timeout")
        .expect("redispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_issue_id.as_str()));
    assert!(
        request.task.contains("fix the edge case with null inputs"),
        "re-dispatched task must contain the reviewer's feedback; got: {}",
        request.task
    );
    assert!(
        request.task.contains("## Original Task"),
        "re-dispatched task must be enriched with history wrapper; got: {}",
        request.task
    );
}
