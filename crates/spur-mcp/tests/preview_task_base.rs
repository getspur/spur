use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use spur_mcp::plan::{labels, PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::plan::{test_util::MockPm, PmLike};
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

fn test_server(repo_root: &std::path::Path) -> (McpCallbackServer, Arc<MockPm>) {
    let session_id = BrainSessionId::new(SessionId::new());
    let continuation_ctx = DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let mock_pm = MockPm::new().arc();
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        pro_feature_gate(),
    );
    server.__test_set_pm_like(Arc::clone(&mock_pm) as Arc<dyn PmLike>);
    server.set_repo_root(repo_root.to_path_buf());
    (server, mock_pm)
}

fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features =
        std::collections::BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
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
            issue_title: None,
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

async fn persist_plan(pm: &MockPm, mut state: PlanState) {
    let epic_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: format!("Epic {}", state.plan_id),
            description: Some("preview fixture".to_string()),
            issue_type: Some("epic".to_string()),
            priority: Some(2),
            labels: vec![
                labels::plan_id(&state.plan_id),
                labels::plan_owner(state.brain_session_id.as_session_id().0.as_str()),
                labels::PLAN_COMPLETE.to_string(),
            ],
            parent: None,
            depends_on: Vec::new(),
            ..Default::default()
        })
        .await
        .expect("create mock epic");
    state.epic_id = Some(epic_id.clone());

    let mut issue_by_task = std::collections::HashMap::new();
    for entry in &state.tasks {
        let depends_on = entry
            .spec
            .depends_on
            .iter()
            .map(|dep| {
                issue_by_task
                    .get(dep)
                    .cloned()
                    .unwrap_or_else(|| panic!("dependency {dep} must be persisted first"))
            })
            .collect();
        let issue_id = pm
            .create_issue(spur_pm::IssueCreate {
                title: format!("Task {}", entry.spec.task_id),
                description: Some(entry.spec.task.clone()),
                issue_type: Some("task".to_string()),
                priority: Some(2),
                labels: vec![
                    labels::plan_id(&state.plan_id),
                    labels::plan_task_id(&entry.spec.task_id),
                    labels::agent(&entry.spec.agent),
                ],
                parent: Some(epic_id.clone()),
                depends_on,
                ..Default::default()
            })
            .await
            .expect("create mock task");
        issue_by_task.insert(entry.spec.task_id.clone(), issue_id.clone());

        if matches!(entry.status, PlanTaskStatus::Approved { .. }) {
            let delegation_id = format!("del-{}", entry.spec.task_id);
            let adv = pm.advanced().expect("mock advanced PM");
            adv.add_comment(
                &issue_id,
                &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
                    delegation_id: delegation_id.clone(),
                    worker: entry.spec.agent.clone(),
                    attempt: entry.attempt,
                }),
            )
            .await
            .expect("seed dispatch audit");
            adv.add_comment(
                &issue_id,
                &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                    delegation_id: delegation_id.clone(),
                    completion_state: CompletionState::AwaitingReview,
                    superseded: false,
                    worker_branch: entry.worker_branch.clone(),
                    result_summary: None,
                    artifact_uri: None,
                    dispatched_base_oid: entry.dispatched_base_oid.clone(),
                }),
            )
            .await
            .expect("seed completion audit");
            adv.add_comment(
                &issue_id,
                &audit_sentinel::encode_comment(&AuditSentinelKind::Approval { delegation_id }),
            )
            .await
            .expect("seed approval audit");
            pm.update_issue(
                &issue_id,
                spur_pm::IssueUpdate {
                    status: Some(pm.closed_status().to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("close approved issue");
        }
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

    let (server, mock_pm) = test_server(dir.path());
    persist_plan(
        &mock_pm,
        plan_state(
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
        ),
    )
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
#[ignore = "TODO bd-d1r-fu-preview-base: legacy approved dep without dispatched_base_oid conflicts with T0 invariant"]
async fn preview_task_base_skips_approved_deps_without_dispatched_base_oid() {
    let dir = init_repo().await;
    let base_oid = git(dir.path(), &["rev-parse", "HEAD"]).await;
    git(
        dir.path(),
        &["branch", "spur/brain-snapshot-preview-skip-legacy", "HEAD"],
    )
    .await;
    git(
        dir.path(),
        &["branch", "spur/worker-preview-skip-legacy-t1", "HEAD"],
    )
    .await;
    let t2_tip = commit_worker_file(
        dir.path(),
        "spur/worker-preview-skip-legacy-t2",
        &base_oid,
        "bar.rs",
        "// t2\n",
    )
    .await;

    let (server, mock_pm) = test_server(dir.path());
    persist_plan(
        &mock_pm,
        plan_state(
            "preview-skip-legacy",
            "spur/brain-snapshot-preview-skip-legacy",
            &base_oid,
            vec![
                task_entry(
                    "T1",
                    Vec::new(),
                    PlanTaskStatus::Approved {
                        summary: Some("legacy t1 approved".into()),
                    },
                    Some("spur/worker-preview-skip-legacy-t1"),
                    None,
                ),
                task_entry(
                    "T2",
                    Vec::new(),
                    PlanTaskStatus::Approved {
                        summary: Some("t2 approved".into()),
                    },
                    Some("spur/worker-preview-skip-legacy-t2"),
                    Some(&base_oid),
                ),
                task_entry("T3", vec!["T1", "T2"], PlanTaskStatus::Pending, None, None),
            ],
        ),
    )
    .await;

    let response = server
        .__test_call_tool(
            "preview_task_base",
            json!({
                "plan_id": "preview-skip-legacy",
                "task_id": "T3",
            }),
        )
        .await;
    let output: Value = serde_json::from_str(&tool_text(&response)).expect("preview JSON");

    assert_eq!(output["overlays"].as_array().expect("overlays").len(), 1);
    assert_eq!(output["overlays"][0]["source_task_id"], "T2");
    assert_eq!(output["overlays"][0]["base_oid"], base_oid);
    assert_eq!(output["overlays"][0]["tip_oid"], t2_tip);
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

    let (server, mock_pm) = test_server(dir.path());
    persist_plan(
        &mock_pm,
        plan_state(
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
        ),
    )
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
