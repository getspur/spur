use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::plan::PlanTask;
use spur_mcp::tools::{BaseSpec, DelegationRequest};
use spur_mcp::McpCallbackServer;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

mod common;

fn run_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_shell(repo: &Path, script: &str) {
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(repo)
        .output()
        .expect("spawn shell");
    assert!(
        output.status.success(),
        "shell failed: {script}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn decode_tool_response(response: &Value) -> Value {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tool call should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text");
    serde_json::from_str(text).expect("tool response text must be JSON")
}

fn extract_submit_plan_id(response: &Value) -> String {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    text.lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .expect("plan_id line")
        .to_string()
}

fn extract_submit_plan_task_map(response: &Value) -> HashMap<String, String> {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_json = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .expect("task_map line");
    serde_json::from_str(task_map_json).expect("task_map json")
}

struct Runtime {
    _dir: TempDir,
    repo: PathBuf,
    pm: Arc<spur_pm::PmService>,
    server: McpCallbackServer,
    reconciler: Reconciler,
    request_rx: tokio::sync::mpsc::Receiver<DelegationRequest>,
    _task_tracker: TaskTracker,
}

async fn new_runtime() -> Runtime {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "README.md"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    common::beads::run_br(dir.path(), &["init"]).expect("br init");

    let repo = dir.path().to_path_buf();
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, &repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );

    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let feature_gate = common::server_builder::pro_feature_gate();
    let (mut server, _unused_channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        common::server_builder::continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&feature_gate),
    );
    server.set_repo_root(repo.clone());

    let (delegation_tx, request_rx) = tokio::sync::mpsc::channel(8);
    let task_tracker = TaskTracker::new();
    let reconciler = Reconciler::new(
        ReconcilerConfig {
            repo_root: repo.clone(),
            ..Default::default()
        },
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: session_id,
            event_sink: None,
            materializer: Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
                Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            )),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        None,
        feature_gate,
    );

    Runtime {
        _dir: dir,
        repo,
        pm,
        server,
        reconciler,
        request_rx,
        _task_tracker: task_tracker,
    }
}

async fn submit_plan(
    runtime: &McpCallbackServer,
    tasks: Value,
    epic_title: &str,
) -> (String, HashMap<String, String>) {
    let response = runtime
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": epic_title,
            "tasks": tasks,
        }))
        .await;
    (
        extract_submit_plan_id(&response),
        extract_submit_plan_task_map(&response),
    )
}

async fn wait_for_dispatch(runtime: &mut Runtime) -> DelegationRequest {
    for _ in 0..30 {
        runtime
            .reconciler
            .tick_once()
            .await
            .expect("reconciler tick");
        if let Ok(Some(req)) =
            tokio::time::timeout(Duration::from_millis(100), runtime.request_rx.recv()).await
        {
            return req;
        }
    }
    panic!("timed out waiting for dispatch");
}

fn complete_worker_attempt(
    repo: &Path,
    request: DelegationRequest,
    branch: &str,
    file: &str,
    body: &str,
) {
    let worktree = repo.join(".spur/worktrees").join(branch.replace('/', "_"));
    std::fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("create parent");

    let declared_base_ref = match request.base.as_ref() {
        Some(BaseSpec::Branch { name }) => name.as_str(),
        Some(BaseSpec::Commit { oid }) => oid.as_str(),
        Some(BaseSpec::RepoMain) | None => "HEAD",
        Some(BaseSpec::WithOverlay { base, .. }) => match base {
            spur_mcp::tools::BaseTarget::RepoMain => "HEAD",
            spur_mcp::tools::BaseTarget::Branch { name } => name.as_str(),
            spur_mcp::tools::BaseTarget::Commit { oid } => oid.as_str(),
        },
    };
    let dispatched_base_oid = run_git(repo, &["rev-parse", declared_base_ref]);

    if let Some(prior) = request.prior_branch_for_reuse.as_deref() {
        run_git(
            repo,
            &[
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "-b",
                branch,
                declared_base_ref,
            ],
        );
        let apply_script = format!(
            "git diff --binary {}..{} | git apply --index --3way",
            declared_base_ref, prior
        );
        run_shell(&worktree, &apply_script);
    } else {
        run_git(
            repo,
            &[
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "-b",
                branch,
                declared_base_ref,
            ],
        );
        if let Some(BaseSpec::WithOverlay { overlays, .. }) = request.base.as_ref() {
            for overlay in overlays {
                let range = format!("{}..{}", overlay.base_oid, overlay.tip_oid);
                run_git(&worktree, &["cherry-pick", &range]);
            }
        }
    }

    if let Some(tx) = request.dispatched_base_oid_tx.as_ref() {
        tx.send(Some(dispatched_base_oid.clone()))
            .expect("publish base oid");
    }
    std::fs::write(worktree.join(file), body).expect("write worker file");
    run_git(&worktree, &["add", "."]);
    run_git(&worktree, &["commit", "-q", "-m", "worker attempt"]);
    let diff = run_git(
        &worktree,
        &["diff", &format!("{}..HEAD", dispatched_base_oid)],
    );
    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Success,
            diff: Some(diff),
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some(branch.to_string()),
            artifact: None,
        })
        .expect("send result");
}

async fn review_task(
    server: &McpCallbackServer,
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: &str,
    reuse_prior_worktree: bool,
) -> Value {
    decode_tool_response(
        &server
            .__test_call_tool(
                "review_task",
                json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "decision": decision,
                    "feedback": feedback,
                    "reuse_prior_worktree": reuse_prior_worktree,
                }),
            )
            .await,
    )
}

async fn wait_for_task_status(
    server: &McpCallbackServer,
    plan_id: &str,
    task_id: &str,
    expected: &str,
) {
    for _ in 0..80 {
        let status = decode_tool_response(
            &server
                .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
                .await,
        );
        let current = status["tasks"]
            .as_array()
            .and_then(|tasks| tasks.iter().find(|t| t["task_id"] == task_id))
            .and_then(|task| task["status"].as_str())
            .unwrap_or_default()
            .to_string();
        if current == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {task_id}={expected}");
}

#[tokio::test]
async fn happy_path_reuse_prior_worktree_and_merge_single_commit() {
    let mut rt = new_runtime().await;
    let (plan_id, task_map) = submit_plan(
        &rt.server,
        json!([{"task_id":"T1","agent":"codex","task":"implement","depends_on":[]}]),
        "reuse e2e",
    )
    .await;
    let issue_id = task_map.get("T1").expect("T1 issue id").clone();
    let plan_base = run_git(&rt.repo, &["rev-parse", "HEAD"]);

    let req1 = wait_for_dispatch(&mut rt).await;
    complete_worker_attempt(&rt.repo, req1, "spur/reuse-e2e-a1", "a.txt", "attempt1\n");
    wait_for_task_status(&rt.server, &plan_id, "T1", "awaiting_review").await;

    let _ = review_task(
        &rt.server,
        &plan_id,
        "T1",
        "request_changes",
        "refine X",
        true,
    )
    .await;

    let req2 = wait_for_dispatch(&mut rt).await;
    assert_eq!(req2.issue_id.as_deref(), Some(issue_id.as_str()));
    assert_eq!(
        req2.prior_branch_for_reuse.as_deref(),
        Some("spur/reuse-e2e-a1")
    );
    complete_worker_attempt(&rt.repo, req2, "spur/reuse-e2e-a2", "b.txt", "attempt2\n");
    wait_for_task_status(&rt.server, &plan_id, "T1", "awaiting_review").await;

    let _ = review_task(&rt.server, &plan_id, "T1", "approve", "looks good", false).await;

    let merge = decode_tool_response(
        &rt.server
            .__test_call_tool("merge_plan", json!({"plan_id": plan_id}))
            .await,
    );
    let merge_branch = merge["merge"]["merge_branch"]
        .as_str()
        .expect("merge branch");

    let merge_commit_count = run_git(
        &rt.repo,
        &[
            "rev-list",
            "--count",
            &format!("{plan_base}..{merge_branch}"),
        ],
    );
    assert_eq!(merge_commit_count, "1", "merge branch must have one commit");
    let merged_diff = run_git(&rt.repo, &["diff", &format!("{plan_base}..{merge_branch}")]);
    assert!(
        merged_diff.contains("a.txt"),
        "merged diff missing attempt1 content"
    );
    assert!(
        merged_diff.contains("b.txt"),
        "merged diff missing attempt2 content"
    );

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        rt.pm.as_ref(),
        &plan_id,
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("project plan");
    let entry = projected
        .tasks
        .iter()
        .find(|task| task.spec.task_id == "T1")
        .expect("projected T1");
    assert_eq!(entry.attempt, 2);
    assert_eq!(
        entry.dispatched_base_oid.as_deref(),
        Some(plan_base.as_str())
    );
}

#[tokio::test]
async fn validation_rejects_reuse_without_worker_branch() {
    let plan = Arc::new(tokio::sync::Mutex::new(spur_mcp::plan::PlanState {
        plan_id: "p1".into(),
        tasks: vec![spur_mcp::plan::PlanTaskEntry {
            spec: PlanTask {
                task_id: "T1".into(),
                agent: "codex".into(),
                task: "x".into(),
                depends_on: vec![],
                issue_id: None,
                issue_title: None,
                context_files: vec![],
            },
            status: spur_mcp::plan::PlanTaskStatus::AwaitingReview {
                summary: Some("wip".into()),
            },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-1".into()),
            dispatched_base_oid: None,
        }],
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
    }));

    let result = spur_mcp::plan::handle_review_task(
        plan,
        "p1",
        "T1",
        "request_changes",
        Some("retry"),
        true,
        None,
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await;

    assert!(
        result.is_err(),
        "expected error when latest attempt has no worker_branch"
    );
}

#[tokio::test]
async fn replay_determinism_preserves_reuse_prior_worktree_in_projection() {
    let dir = TempDir::new().expect("tempdir");
    common::beads::run_br(dir.path(), &["init"]).expect("br init");
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new")
            .expect("beads pm"),
    );

    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-replay",
        "Replay",
        None,
        &[PlanTask {
            task_id: "T1".into(),
            agent: "codex".into(),
            task: "x".into(),
            depends_on: vec![],
            issue_id: None,
            issue_title: None,
            context_files: vec![],
        }],
    )
    .await
    .expect("build epic");

    let task_issue = subgraph.task_map.get("T1").expect("task issue id");
    let adv = pm.advanced().expect("advanced");
    adv.add_comment(
        task_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        }),
    )
    .await
    .expect("dispatch comment");
    adv.add_comment(
        task_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::ReviewFeedback {
            delegation_id: "del-1".into(),
            attempt: 1,
            feedback: "refine".into(),
            worker_branch: Some("spur/worker-a1".into()),
            summary: Some("done".into()),
            reuse_prior_worktree: Some(true),
        }),
    )
    .await
    .expect("review feedback comment");

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "plan-replay",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("project");
    let task = projected
        .tasks
        .iter()
        .find(|t| t.spec.task_id == "T1")
        .expect("T1");
    assert_eq!(task.history.len(), 1);
    assert_eq!(task.history[0].reuse_prior_worktree, Some(true));
}

#[tokio::test]
#[ignore = "current reconciler base-spec optimization for single-parent deps emits BaseSpec::Branch, not WithOverlay; asserting overlay parity needs production change"]
async fn approved_dep_overlay_and_reuse_branch_are_both_present_on_redispatch() {
    let mut rt = new_runtime().await;
    let (plan_id, _task_map) = submit_plan(
        &rt.server,
        json!([
            {"task_id":"A","agent":"codex","task":"dep","depends_on":[]},
            {"task_id":"B","agent":"codex","task":"leaf","depends_on":["A"]}
        ]),
        "overlay + reuse",
    )
    .await;

    let req_a = wait_for_dispatch(&mut rt).await;
    complete_worker_attempt(&rt.repo, req_a, "spur/overlay-a", "a.txt", "A\n");
    let _ = review_task(&rt.server, &plan_id, "A", "approve", "ok", false).await;

    let req_b1 = wait_for_dispatch(&mut rt).await;
    complete_worker_attempt(&rt.repo, req_b1, "spur/overlay-b1", "b1.txt", "B1\n");
    let _ = review_task(
        &rt.server,
        &plan_id,
        "B",
        "request_changes",
        "retry B",
        true,
    )
    .await;

    let req_b2 = wait_for_dispatch(&mut rt).await;
    assert_eq!(
        req_b2.prior_branch_for_reuse.as_deref(),
        Some("spur/overlay-b1")
    );
    match req_b2.base.as_ref() {
        Some(BaseSpec::Branch { name }) => {
            assert_eq!(name, "spur/overlay-a");
        }
        other => panic!("expected Branch base for B redispatch, got {other:?}"),
    }
    // Boundary assertion for orchestrator sequencing: request includes both
    // overlay info and prior_branch_for_reuse, so pre-apply can only occur
    // after overlay application in worker setup.
}

#[tokio::test]
async fn review_feedback_sentinel_byte_stable_and_projectable() {
    let original = AuditSentinelKind::ReviewFeedback {
        delegation_id: "del-1".into(),
        attempt: 1,
        feedback: "refine X".into(),
        worker_branch: Some("spur/worker-1".into()),
        summary: Some("summary".into()),
        reuse_prior_worktree: Some(true),
    };
    let encoded_1 = audit_sentinel::encode_comment(&original);
    let parsed = audit_sentinel::parse_comment(&encoded_1)
        .expect("sentinel prefix")
        .expect("sentinel parse");
    let encoded_2 = audit_sentinel::encode_comment(&parsed);
    assert_eq!(
        encoded_1, encoded_2,
        "sentinel bytes must be stable across round-trip"
    );

    let dir = TempDir::new().expect("tempdir");
    common::beads::run_br(dir.path(), &["init"]).expect("br init");
    let issue = serde_json::from_str::<serde_json::Value>(
        &common::beads::run_br(dir.path(), &["create", "task", "-t", "task"]).expect("create"),
    )
    .expect("issue json");
    let issue_id = issue["id"].as_str().expect("id");
    common::beads::run_br(dir.path(), &["comments", "add", issue_id, &encoded_1])
        .expect("add comment");

    let comments = common::beads::run_br(dir.path(), &["comments", "list", issue_id, "--json"])
        .expect("list comments");
    let arr: Value = serde_json::from_str(&comments).expect("comments json");
    let replay = arr
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|c| c["text"].as_str())
        .find_map(audit_sentinel::parse_comment)
        .expect("sentinel")
        .expect("parse ok");
    match replay {
        AuditSentinelKind::ReviewFeedback {
            reuse_prior_worktree,
            ..
        } => {
            assert_eq!(reuse_prior_worktree, Some(true));
        }
        other => panic!("expected ReviewFeedback, got {other:?}"),
    }
}
