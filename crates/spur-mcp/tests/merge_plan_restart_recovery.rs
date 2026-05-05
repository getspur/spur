use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{encode_comment, AuditSentinelKind, CompletionState};
use spur_mcp::plan::{labels, PlanTask};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::{build_epic_subgraph, emit_plan_submit_audit};
use spur_pm::{IssueUpdate, PmService};
use tempfile::TempDir;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

async fn run_git_capture(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed (exit {}): stderr={} stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn git_object_exists(repo: &Path, object: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", object])
        .current_dir(repo)
        .output()
        .expect("git cat-file invocation failed")
        .status
        .success()
}

async fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git_capture(dir.path(), &["init", "-q"]).await;
    run_git_capture(dir.path(), &["config", "user.email", "test@spur"]).await;
    run_git_capture(dir.path(), &["config", "user.name", "spur-test"]).await;
    dir
}

async fn commit_file(repo: &Path, path: &str, body: &str, message: &str) {
    std::fs::write(repo.join(path), body).expect("write file");
    run_git_capture(repo, &["add", path]).await;
    run_git_capture(repo, &["commit", "-q", "-m", message]).await;
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn decode_tool_response(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "tool call should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text");
    serde_json::from_str(text).expect("tool response must be json")
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn t_v0d_3_merge_plan_works_after_restart_on_persisted_plan() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = init_repo().await;
    run_br(dir.path(), &["init"]).expect("br init");
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;

    let pm = beads_pm(dir.path()).await;
    let plan_id = "plan-merge-restart";
    run_git_capture(dir.path(), &["branch", "spur/brain-snapshot-test", "HEAD"]).await;
    let base_snapshot_oid = run_git_capture(
        dir.path(),
        &["rev-parse", "--verify", "spur/brain-snapshot-test"],
    )
    .await;

    run_git_capture(
        dir.path(),
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a",
            "spur/brain-snapshot-test",
        ],
    )
    .await;
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), &["checkout", "-q", "spur/brain-snapshot-test"]).await;
    commit_file(
        dir.path(),
        "post-snapshot.txt",
        "advanced\n",
        "advance snapshot after capture",
    )
    .await;

    let tasks = vec![PlanTask {
        task_id: "task-a".into(),
        agent: "codex".into(),
        task: "Integrate worker branch".into(),
        depends_on: Vec::new(),
        issue_id: None,
        context_files: Vec::new(),
    }];
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Restart Recovery Epic",
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");
    emit_plan_submit_audit(
        pm.advanced().expect("advanced beads backend"),
        plan_id,
        &subgraph,
        Some("spur/brain-snapshot-test"),
        Some(base_snapshot_oid.as_str()),
        Some("submit_plan"),
        Some(&spur_acp::SessionId("brain".into())),
    )
    .await;

    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    let adv = pm.advanced().expect("advanced beads backend");
    adv.add_comment(
        &task_issue_id,
        &encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-a".into()),
            result_summary: Some("worker branch ready".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .expect("completion audit");
    adv.add_comment(
        &task_issue_id,
        &encode_comment(&AuditSentinelKind::Approval {
            delegation_id: "del-1".into(),
        }),
    )
    .await
    .expect("approval audit");
    pm.update_issue(
        &task_issue_id,
        IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task issue");
    pm.update_issue(
        &subgraph.epic_id,
        IssueUpdate {
            add_labels: vec![
                labels::INTEGRATION_PENDING.to_string(),
                labels::plan_owner("brain"),
            ],
            ..Default::default()
        },
    )
    .await
    .expect("add integration-pending label");

    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server1, _channel1) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server1.set_repo_root(dir.path().to_path_buf());
    let warm_status = server1
        .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
        .await;
    assert!(
        warm_status.get("error").is_none(),
        "server1 should be able to project the persisted plan before restart: {warm_status}"
    );
    assert!(
        server1.__test_active_plan_count().await > 0,
        "warming server1 should populate its plan cache"
    );
    drop(server1);

    let (mut server2, _channel2) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server2.set_repo_root(dir.path().to_path_buf());
    assert_eq!(
        server2.__test_active_plan_count().await,
        0,
        "fresh server must start with an empty active_plans cache"
    );
    let epic_before_merge = pm.get_issue(&subgraph.epic_id).await.expect("get epic");
    assert!(
        epic_before_merge
            .labels
            .iter()
            .any(|label| label == labels::INTEGRATION_PENDING),
        "integration-pending must exist before merge_plan runs: {:?}",
        epic_before_merge.labels
    );

    let status = decode_tool_response(
        &server2
            .__test_call_tool("merge_plan", json!({ "plan_id": plan_id }))
            .await,
    );

    assert_eq!(status["merge"]["status"], "succeeded");
    assert_eq!(status["merge"]["merged_task_ids"], json!(["task-a"]));
    assert!(
        server2.__test_active_plan_count().await > 0,
        "restart recovery should repopulate server2 cache from persisted state"
    );
    let merge_branch = status["merge"]["merge_branch"]
        .as_str()
        .expect("merge branch in merge_plan response");
    assert_eq!(
        run_git_capture(dir.path(), &["show", &format!("{merge_branch}:worker.txt")]).await,
        "worker",
        "merge_plan should create a branch containing the worker change"
    );
    assert!(
        !git_object_exists(dir.path(), &format!("{merge_branch}:post-snapshot.txt")).await,
        "merge_plan must integrate from the persisted base snapshot OID, not the advanced branch head"
    );

    let epic = pm.get_issue(&subgraph.epic_id).await.expect("get epic");
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label == labels::INTEGRATION_PENDING),
        "merge_plan should clear integration-pending after restart recovery: {:?}",
        epic.labels
    );
}
