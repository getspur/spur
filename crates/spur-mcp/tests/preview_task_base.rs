use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::{PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::McpCallbackServer;
use tempfile::TempDir;

async fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .await
        .expect("git command should spawn");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]).await;
    git(dir.path(), &["config", "user.email", "test@spur"]).await;
    git(dir.path(), &["config", "user.name", "spur-test"]).await;
    std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
    git(dir.path(), &["add", "README.md"]).await;
    git(dir.path(), &["commit", "-q", "-m", "seed"]).await;
    dir
}

async fn commit_worker_file(
    repo: &std::path::Path,
    branch: &str,
    base_ref: &str,
    path: &str,
    content: &str,
) -> String {
    git(repo, &["checkout", "-q", "-B", branch, base_ref]).await;
    std::fs::write(repo.join(path), content).expect("write worker file");
    git(repo, &["add", path]).await;
    git(repo, &["commit", "-q", "-m", &format!("write {path}")]).await;
    let tip = git(repo, &["rev-parse", "HEAD"]).await;
    git(repo, &["checkout", "-q", "main"]).await;
    tip
}

fn test_server(repo_root: &std::path::Path) -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId::new());
    let continuation_ctx = DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mut server, _channel) = McpCallbackServer::new(
        &session_id,
        None,
        None,
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    server.set_repo_root(repo_root.to_path_buf());
    server
}

fn task_entry(
    task_id: &str,
    depends_on: Vec<&str>,
    status: PlanTaskStatus,
    worker_branch: Option<&str>,
    dispatched_base_oid: Option<&str>,
) -> PlanTaskEntry {
    PlanTaskEntry {
        spec: PlanTask {
            task_id: task_id.to_string(),
            agent: "codex".to_string(),
            task: format!("task {task_id}"),
            depends_on: depends_on.into_iter().map(str::to_string).collect(),
            issue_id: None,
            context_files: Vec::new(),
        },
        status,
        result: None,
        worker_branch: worker_branch.map(str::to_string),
        attempt: 1,
        history: Vec::new(),
        last_delegation_id: None,
        dispatched_base_oid: dispatched_base_oid.map(str::to_string),
    }
}

fn plan_state(
    plan_id: &str,
    base_snapshot_branch: &str,
    base_snapshot_oid: &str,
    tasks: Vec<PlanTaskEntry>,
) -> PlanState {
    PlanState {
        plan_id: plan_id.to_string(),
        tasks,
        brain_session_id: BrainSessionId::new(SessionId::new()),
        base_snapshot_branch: Some(base_snapshot_branch.to_string()),
        base_snapshot_oid: Some(base_snapshot_oid.to_string()),
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
    }
}

fn tool_text(response: &Value) -> String {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tool call should succeed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text")
        .to_string()
}

async fn assert_no_preview_refs(repo: &std::path::Path) {
    let branches = git(repo, &["branch", "--list", "spur/preview-*"]).await;
    assert!(
        branches.trim().is_empty(),
        "preview branch should be removed, found: {branches}"
    );

    let worktrees = git(repo, &["worktree", "list", "--porcelain"]).await;
    assert!(
        !worktrees.contains(".spur/worktrees/preview"),
        "preview worktree should be removed, worktrees: {worktrees}"
    );
}

#[tokio::test]
async fn preview_task_base_returns_overlays_and_base_oid_when_clean() {
    let dir = init_repo().await;
    let base_oid = git(dir.path(), &["rev-parse", "HEAD"]).await;
    git(
        dir.path(),
        &["branch", "spur/brain-snapshot-preview-clean", "HEAD"],
    )
    .await;
    let worker_tip = commit_worker_file(
        dir.path(),
        "spur/worker-preview-clean-t1",
        &base_oid,
        "foo.rs",
        "// t1\n",
    )
    .await;

    let server = test_server(dir.path());
    server
        .__test_install_plan(plan_state(
            "preview-clean",
            "spur/brain-snapshot-preview-clean",
            &base_oid,
            vec![
                task_entry(
                    "T1",
                    Vec::new(),
                    PlanTaskStatus::Approved {
                        summary: Some("t1 approved".into()),
                    },
                    Some("spur/worker-preview-clean-t1"),
                    Some(&base_oid),
                ),
                task_entry("T2", vec!["T1"], PlanTaskStatus::Pending, None, None),
            ],
        ))
        .await;

    let response = server
        .__test_call_tool(
            "preview_task_base",
            json!({
                "plan_id": "preview-clean",
                "task_id": "T2",
            }),
        )
        .await;
    let output: Value = serde_json::from_str(&tool_text(&response)).expect("preview JSON");

    assert_eq!(output["overlays"].as_array().expect("overlays").len(), 1);
    assert_eq!(output["overlays"][0]["source_task_id"], "T1");
    assert_eq!(output["overlays"][0]["base_oid"], base_oid);
    assert_eq!(output["overlays"][0]["tip_oid"], worker_tip);
    assert!(
        output["predicted_base_oid"].as_str().is_some(),
        "clean preview should return predicted base oid: {output}"
    );
    assert!(output["conflict"].is_null());
    assert_no_preview_refs(dir.path()).await;
}

#[tokio::test]
async fn preview_task_base_reports_conflict_when_overlays_collide() {
    let dir = init_repo().await;
    std::fs::write(dir.path().join("foo.rs"), "base\n").expect("write shared file");
    git(dir.path(), &["add", "foo.rs"]).await;
    git(dir.path(), &["commit", "-q", "-m", "shared base"]).await;
    let base_oid = git(dir.path(), &["rev-parse", "HEAD"]).await;
    git(
        dir.path(),
        &["branch", "spur/brain-snapshot-preview-conflict", "HEAD"],
    )
    .await;
    let t1_tip = commit_worker_file(
        dir.path(),
        "spur/worker-preview-conflict-t1",
        &base_oid,
        "foo.rs",
        "T1\n",
    )
    .await;
    let t2_tip = commit_worker_file(
        dir.path(),
        "spur/worker-preview-conflict-t2",
        &base_oid,
        "foo.rs",
        "T2\n",
    )
    .await;

    let server = test_server(dir.path());
    server
        .__test_install_plan(plan_state(
            "preview-conflict",
            "spur/brain-snapshot-preview-conflict",
            &base_oid,
            vec![
                task_entry(
                    "T1",
                    Vec::new(),
                    PlanTaskStatus::Approved {
                        summary: Some("t1 approved".into()),
                    },
                    Some("spur/worker-preview-conflict-t1"),
                    Some(&base_oid),
                ),
                task_entry(
                    "T2",
                    Vec::new(),
                    PlanTaskStatus::Approved {
                        summary: Some("t2 approved".into()),
                    },
                    Some("spur/worker-preview-conflict-t2"),
                    Some(&base_oid),
                ),
                task_entry("T3", vec!["T1", "T2"], PlanTaskStatus::Pending, None, None),
            ],
        ))
        .await;

    let response = server
        .__test_call_tool(
            "preview_task_base",
            json!({
                "plan_id": "preview-conflict",
                "task_id": "T3",
            }),
        )
        .await;
    let output: Value = serde_json::from_str(&tool_text(&response)).expect("preview JSON");

    assert_eq!(output["overlays"].as_array().expect("overlays").len(), 2);
    assert_eq!(output["overlays"][0]["source_task_id"], "T1");
    assert_eq!(output["overlays"][0]["tip_oid"], t1_tip);
    assert_eq!(output["overlays"][1]["source_task_id"], "T2");
    assert_eq!(output["overlays"][1]["tip_oid"], t2_tip);
    assert!(output["predicted_base_oid"].is_null());
    assert_eq!(output["conflict"]["dep_task_id"], "T2");
    let files = output["conflict"]["files"]
        .as_array()
        .expect("conflict files");
    assert!(
        files.iter().any(|file| file == "foo.rs"),
        "conflict files should include foo.rs: {files:?}"
    );
    assert_no_preview_refs(dir.path()).await;
}
