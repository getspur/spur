use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use tempfile::TempDir;

fn no_op_ctx() -> super::DetachedContinuationCtx {
    super::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

async fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
        .await
        .expect("git init");
    super::run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
        .await
        .expect("git config user.email");
    super::run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
        .await
        .expect("git config user.name");
    std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
    super::run_git_capture(dir.path(), None, &["add", "README.md"])
        .await
        .expect("git add seed");
    super::run_git_capture(dir.path(), None, &["commit", "-q", "-m", "seed"])
        .await
        .expect("git commit seed");
    dir
}

async fn commit_worker_file(
    repo: &std::path::Path,
    branch: &str,
    path: &str,
    content: String,
) -> String {
    super::run_git_capture(repo, None, &["checkout", "-q", "-B", branch, "main"])
        .await
        .expect("checkout worker branch");
    std::fs::write(repo.join(path), content).expect("write worker file");
    super::run_git_capture(repo, None, &["add", path])
        .await
        .expect("git add worker file");
    super::run_git_capture(
        repo,
        None,
        &["commit", "-q", "-m", &format!("write {path}")],
    )
    .await
    .expect("git commit worker file");
    super::run_git_capture(repo, None, &["rev-parse", "HEAD"])
        .await
        .expect("git rev-parse worker tip")
}

fn task_entry(
    task_id: &str,
    status: crate::plan::PlanTaskStatus,
    worker_branch: &str,
    dispatched_base_oid: &str,
) -> crate::plan::PlanTaskEntry {
    crate::plan::PlanTaskEntry {
        spec: crate::plan::PlanTask {
            task_id: task_id.to_string(),
            agent: "codex".to_string(),
            task: format!("task {task_id}"),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        },
        status,
        result: None,
        worker_branch: Some(worker_branch.to_string()),
        attempt: 1,
        history: Vec::new(),
        last_delegation_id: Some(format!("del-{task_id}")),
        dispatched_base_oid: Some(dispatched_base_oid.to_string()),
    }
}

#[tokio::test]
async fn clobber_detector_for_review_uses_approved_branch_tip_not_dispatched_base_oid() {
    let dir = init_repo().await;
    let base_oid = super::run_git_capture(dir.path(), None, &["rev-parse", "main"])
        .await
        .expect("git rev-parse main");
    let worker_a_tip = commit_worker_file(
        dir.path(),
        "spur/test-clobber-worker-a",
        "foo.rs",
        "A".repeat(200),
    )
    .await;
    let worker_b_tip = commit_worker_file(
        dir.path(),
        "spur/test-clobber-worker-b",
        "foo.rs",
        "B".repeat(200),
    )
    .await;

    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, _channel) = super::McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        no_op_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        super::community_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let plan_arc = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
        plan_id: "plan-clobber".to_string(),
        tasks: vec![
            task_entry(
                "T1",
                crate::plan::PlanTaskStatus::Approved {
                    summary: Some("approved".to_string()),
                },
                "spur/test-clobber-worker-a",
                &base_oid,
            ),
            task_entry(
                "T2",
                crate::plan::PlanTaskStatus::AwaitingReview {
                    summary: Some("awaiting review".to_string()),
                },
                "spur/test-clobber-worker-b",
                &base_oid,
            ),
        ],
        brain_session_id: session_id,
        base_snapshot_branch: Some("main".to_string()),
        base_snapshot_oid: Some(base_oid.clone()),
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: None,
    }));

    let report = server
        .run_clobber_detector_for_review(&plan_arc, "T2")
        .await
        .expect("clobber detector review");

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.signals.len(), 1, "{:?}", report.signals);
    match &report.signals[0] {
        crate::plan::signals::WorkerSignal::PotentialClobber {
            conflicting_task_id,
            file,
            upstream_tip,
            worker_tip,
            ..
        } => {
            assert_eq!(conflicting_task_id, "T1");
            assert_eq!(file, "foo.rs");
            assert_eq!(upstream_tip, &worker_a_tip);
            assert_eq!(worker_tip, &worker_b_tip);
            assert_ne!(
                upstream_tip, &base_oid,
                "prior tip must be the approved branch tip, not dispatched_base_oid"
            );
        }
        signal => panic!("expected PotentialClobber signal, got {signal:?}"),
    }
}
