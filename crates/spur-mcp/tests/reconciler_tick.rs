//! Integration tests for `Reconciler`.
//!
//! # `observe_ready_returns_unblocked_task_only`
//!
//! Exercises the `observe_ready` bv-primary path (falls through to br if bv
//! absent). Requires `br` on PATH; skipped otherwise.
//!
//! Setup:
//!   - temp beads workspace, `br init`
//!   - create epic + 2 tasks; task B depends on task A
//!   - label all three with `spur:plan-id:P1`
//!   - label the epic with `spur:plan-complete`
//!
//! Assertion:
//!   - `observe_ready` returns a list containing task A's id
//!   - task B is NOT in the list (blocked by A)
//!
//! # `observe_ready_via_br_returns_ready_tasks`
//!
//! Exercises the br fallback path (`observe_ready_via_br`) directly, bypassing
//! the bv primary path entirely. Verifies the task-level ready query is
//! followed by the epic activation guard.
//!
//! Same fixture as above; calls `observe_ready_via_br()` instead of
//! `observe_ready()`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_acp::{
    BrainSessionId, DelegationResult, DelegationStatus, SessionId, SpurEvent, SpurEventBody,
};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::reconciler::{
    PlanDispatchState, Reconciler, ReconcilerConfig, ReconcilerDispatchCtx,
};
use spur_mcp::plan::{labels, PlanTask};
use spur_mcp::tools::BaseSpec;
use spur_mcp::McpEventSink;
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

/// Run `br <args> --json` in the given directory; panics on failure.
fn run_br_json(repo: &Path, args: &[&str]) -> String {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
}

/// Run `br <args>` in the given directory (no --json); panics on failure.
fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

/// Run `git <args>` in the given directory; panics on failure.
fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "git {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

fn init_git_repo(repo: &Path) {
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.email", "test@example.com"]);
    run_git(repo, &["config", "user.name", "SPUR Test"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("write README");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "base"]);
}

/// Extract the `"id"` field from a JSON object returned by `br create --json`.
fn parse_id_from_create(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("br create output not JSON");
    v.get("id")
        .and_then(|id| id.as_str())
        .unwrap_or_else(|| panic!("no 'id' field in br create output: {json}"))
        .to_string()
}

/// Apply a label to an issue via `br label add <id> <label>`.
fn label_issue(repo: &Path, issue_id: &str, label: &str) {
    run_br(repo, &["label", "add", issue_id, label]);
}

fn plan_task(task_id: &str) -> PlanTask {
    PlanTask {
        task_id: task_id.to_string(),
        agent: "codex".to_string(),
        task: format!("Do {task_id}."),
        depends_on: Vec::new(),
        issue_id: None,
        context_files: Vec::new(),
    }
}

fn collect_sentinels(list_json: &str) -> Vec<AuditSentinelKind> {
    let items: serde_json::Value =
        serde_json::from_str(list_json).expect("br comments list must be valid JSON");
    items
        .as_array()
        .expect("comments must be JSON array")
        .iter()
        .filter_map(|comment| comment.get("text").and_then(|text| text.as_str()))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(|result| result.ok())
        .collect()
}

fn extract_submit_plan_task_issue_id(response: &serde_json::Value, task_id: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_json = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .expect("submit_plan response must include task_map line");
    let task_map: std::collections::HashMap<String, String> =
        serde_json::from_str(task_map_json).expect("task_map line must be valid JSON");
    task_map
        .get(task_id)
        .cloned()
        .unwrap_or_else(|| panic!("submit_plan task_map must include '{task_id}'"))
}

fn dependent_plan_tasks() -> Vec<PlanTask> {
    vec![
        PlanTask {
            task_id: "T1".to_string(),
            agent: "codex".to_string(),
            task: "Build dependency output.".to_string(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "T2".to_string(),
            agent: "codex".to_string(),
            task: "Use dependency output.".to_string(),
            depends_on: vec!["T1".to_string()],
            issue_id: None,
            context_files: Vec::new(),
        },
    ]
}

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEvent>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events.lock().unwrap().push(SpurEvent::now(body));
    }
}

#[tokio::test]
async fn tick_once_does_not_dispatch_partial_plan_after_child_create_failure() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);

    let tasks = vec![
        plan_task("t1"),
        plan_task("t2"),
        // Slash is illegal in beads labels. `build_epic_subgraph` reaches this
        // third child create before `br create` rejects spur:plan-task-id:bad/task.
        plan_task("bad/task"),
    ];
    let err = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "partial-plan",
        "Partial Plan",
        None,
        &tasks,
    )
    .await
    .expect_err("third child create must fail");
    assert!(
        err.contains("failed to create child"),
        "unexpected build error: {err}"
    );

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(8);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("partial-plan".into()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = tokio::time::timeout(std::time::Duration::from_secs(5), reconciler.tick_once())
        .await
        .expect("tick_once must not hang")
        .expect("tick_once");
    assert!(!did_work, "partial plan must not produce dispatch work");
    assert!(
        delegation_rx.try_recv().is_err(),
        "partial plan must not enqueue a delegation"
    );
}

#[tokio::test]
async fn tick_once_skips_plan_owned_by_another_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "other-owned-plan",
        "Other Owned Plan",
        None,
        &[plan_task("t1")],
    )
    .await
    .expect("build_epic_subgraph");
    label_issue(
        dir.path(),
        &subgraph.epic_id,
        &labels::plan_owner("other-brain"),
    );

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("other-owned-plan".to_string()),
        feature_gate,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(!did_work, "other-owned plan must not produce dispatch work");
    assert!(
        delegation_rx.try_recv().is_err(),
        "other-owned plan must not enqueue a delegation"
    );
}

#[tokio::test]
async fn tick_once_skips_terminal_epic_owned_by_another_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Other Terminal Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("other-terminal"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("other-brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Terminal Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("other-terminal"));
    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task");

    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("other-terminal".to_string()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(!did_work, "non-owner terminal epic must not be reconciled");
    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, "open");

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &epic_id]));
    assert!(!sentinels
        .iter()
        .any(|audit| matches!(audit, AuditSentinelKind::EpicCompletion { .. })));
}

#[tokio::test]
async fn tick_once_does_not_reclaim_expired_lease_owned_by_another_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Other Lease Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("other-lease"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("other-brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Other Leased Task",
            "--priority",
            "2",
        ],
    ));
    let delegation_id = "del-other-expired";
    let expired_at = 1;
    label_issue(dir.path(), &task_id, &labels::plan_id("other-lease"));
    label_issue(
        dir.path(),
        &task_id,
        &labels::plan_task_id("t-other-expired"),
    );
    label_issue(dir.path(), &task_id, &labels::agent("codex"));
    label_issue(dir.path(), &task_id, &labels::delegation_id(delegation_id));
    label_issue(dir.path(), &task_id, &labels::lease_expires_at(expired_at));

    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let event_sink: Arc<dyn McpEventSink> = sink.clone();
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            event_sink: Some(event_sink),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("other-lease".to_string()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(!did_work, "non-owner expired lease must not be reclaimed");
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, "open");
    assert!(issue.labels.contains(&labels::delegation_id(delegation_id)));
    assert!(issue.labels.contains(&labels::lease_expires_at(expired_at)));

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &task_id]));
    assert!(!sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion { .. } | AuditSentinelKind::DispatchOrphanCleared { .. }
    )));
    let events = sink.events.lock().expect("events lock");
    assert!(!events
        .iter()
        .any(|event| matches!(event.body, SpurEventBody::DispatchLeaseExpired { .. })));
}

#[tokio::test]
async fn tick_once_dispatches_ready_task_with_single_approved_dep_branch_base() {
    let dir = TempDir::new().expect("tempdir");
    init_git_repo(dir.path());
    let worker_branch = "spur-test-t1-worker";
    run_git(dir.path(), &["branch", worker_branch]);
    run_br(dir.path(), &["init"]);

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "overlay-plan",
        "Overlay Plan",
        None,
        &dependent_plan_tasks(),
    )
    .await
    .expect("build_epic_subgraph");
    label_issue(dir.path(), &subgraph.epic_id, &labels::plan_owner("brain"));
    let task_1_issue = subgraph.task_map["T1"].clone();
    let task_2_issue = subgraph.task_map["T2"].clone();

    let adv = pm.advanced().expect("beads backend");
    spur_mcp::emit_plan_submit_audit(
        adv,
        "overlay-plan",
        &subgraph,
        Some("spur/plan-base-overlay"),
        Some("base-snapshot-oid"),
        None,
        Some(&SessionId("brain".to_string())),
        None,
    )
    .await;
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
            delegation_id: "del-t1".to_string(),
            worker: "codex".to_string(),
            attempt: 1,
        }),
    )
    .await
    .expect("dispatch audit");
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-t1".to_string(),
            completion_state: audit_sentinel::CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.to_string()),
            result_summary: Some("T1 complete".to_string()),
            artifact_uri: None,
            dispatched_base_oid: Some("t1-dispatched-base".to_string()),
        }),
    )
    .await
    .expect("completion audit");
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
            delegation_id: "del-t1".to_string(),
        }),
    )
    .await
    .expect("approval audit");
    pm.update_issue(
        &task_1_issue,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close approved T1");

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let reconciler = Reconciler::new(
        ReconcilerConfig {
            repo_root: dir.path().to_path_buf(),
            ..Default::default()
        },
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("overlay-plan".to_string()),
        feature_gate,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(did_work, "T2 should be dispatched");
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), delegation_rx.recv())
        .await
        .expect("delegation request timeout")
        .expect("delegation request");

    assert!(
        request.dispatched_base_oid_tx.is_some(),
        "reconciler must pass dispatched_base_oid watch sender to orchestrator"
    );
    let dispatched_base_oid_tx = request
        .dispatched_base_oid_tx
        .clone()
        .expect("dispatched_base_oid sender");
    let retry_dispatched_base_oid_tx = dispatched_base_oid_tx.clone();
    dispatched_base_oid_tx
        .send(Some("attempt-1-base".to_string()))
        .expect("first base oid send");
    retry_dispatched_base_oid_tx
        .send(Some("attempt-2-base".to_string()))
        .expect("retry base oid send");
    let base = request.base.expect("plan dispatch must pass BaseSpec");
    match base {
        BaseSpec::Branch { name } => {
            assert_eq!(name, worker_branch);
        }
        other => panic!("expected single-parent branch BaseSpec, got {other:?}"),
    }

    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("T2 complete".to_string()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-t2".to_string()),
            artifact: None,
        })
        .expect("send delegation result");
    task_tracker.close();
    task_tracker.wait().await;

    let sentinels = collect_sentinels(&run_br_json(
        dir.path(),
        &["comments", "list", &task_2_issue],
    ));
    let completion_base = sentinels.iter().find_map(|sentinel| match sentinel {
        AuditSentinelKind::Completion {
            delegation_id,
            dispatched_base_oid,
            ..
        } if delegation_id != "del-t1" => dispatched_base_oid.as_deref(),
        _ => None,
    });
    assert_eq!(
        completion_base,
        Some("attempt-2-base"),
        "completion audit must persist the successful retry attempt's dispatched base"
    );
}

#[tokio::test]
async fn overlay_conflict_routes_to_blocked_on_setup_conflict() {
    let dir = TempDir::new().expect("tempdir");
    init_git_repo(dir.path());
    let worker_branch = "spur-test-t1-worker";
    run_git(dir.path(), &["branch", worker_branch]);
    run_br(dir.path(), &["init"]);

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "overlay-conflict-plan",
        "Overlay Conflict Plan",
        None,
        &dependent_plan_tasks(),
    )
    .await
    .expect("build_epic_subgraph");
    label_issue(dir.path(), &subgraph.epic_id, &labels::plan_owner("brain"));
    let task_1_issue = subgraph.task_map["T1"].clone();
    let task_2_issue = subgraph.task_map["T2"].clone();

    let adv = pm.advanced().expect("beads backend");
    spur_mcp::emit_plan_submit_audit(
        adv,
        "overlay-conflict-plan",
        &subgraph,
        Some("spur/plan-base-overlay-conflict"),
        Some("base-snapshot-oid"),
        None,
        Some(&SessionId("brain".to_string())),
        None,
    )
    .await;
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
            delegation_id: "del-t1".to_string(),
            worker: "codex".to_string(),
            attempt: 1,
        }),
    )
    .await
    .expect("dispatch audit");
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-t1".to_string(),
            completion_state: audit_sentinel::CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.to_string()),
            result_summary: Some("T1 complete".to_string()),
            artifact_uri: None,
            dispatched_base_oid: Some("t1-dispatched-base".to_string()),
        }),
    )
    .await
    .expect("completion audit");
    adv.add_comment(
        &task_1_issue,
        &audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
            delegation_id: "del-t1".to_string(),
        }),
    )
    .await
    .expect("approval audit");
    pm.update_issue(
        &task_1_issue,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close approved T1");

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let reconciler = Reconciler::new(
        ReconcilerConfig {
            repo_root: dir.path().to_path_buf(),
            ..Default::default()
        },
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".to_string())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("overlay-conflict-plan".to_string()),
        feature_gate,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(did_work, "T2 should be dispatched");
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), delegation_rx.recv())
        .await
        .expect("delegation request timeout")
        .expect("delegation request");

    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::SetupFailed {
                error: spur_acp::AttemptSetupError::OverlayConflict {
                    source_task_id: "T1".to_string(),
                    files: vec!["foo.rs".to_string()],
                },
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send setup conflict result");
    task_tracker.close();
    task_tracker.wait().await;

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "overlay-conflict-plan",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("project plan");
    let task = projected
        .tasks
        .iter()
        .find(|task| task.spec.issue_id.as_deref() == Some(task_2_issue.as_str()))
        .expect("projected task");
    assert!(matches!(
        &task.status,
        spur_mcp::plan::PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files }
            if dep_task_id == "T1" && files == &vec!["foo.rs".to_string()]
    ));

    let issue = pm.get_issue(&task_2_issue).await.expect("get T2 issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == "signal:integration-conflict"),
        "T2 should carry the integration conflict signal label; labels={:?}",
        issue.labels
    );
}

#[tokio::test]
async fn observe_ready_returns_unblocked_task_only() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // --- Create epic + 2 tasks ---
    let epic_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan P1 Epic",
            "--priority",
            "2",
        ],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A (unblocked)",
            "--priority",
            "2",
        ],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B (blocked by A)",
            "--priority",
            "2",
        ],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    // --- Task B depends on Task A (B is blocked by A) ---
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    // --- Label all three with plan-id:P1 ---
    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);

    // --- Label epic with spur:plan-complete ---
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    // --- Construct PmService ---
    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    // --- Build and invoke reconciler ---
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let ready_ids = reconciler
        .observe_ready()
        .await
        .expect("observe_ready must not fail");

    // Task A is unblocked — must appear.
    assert!(
        ready_ids.contains(&task_a_id),
        "expected task A ({task_a_id}) in ready list; got: {ready_ids:?}"
    );

    // Task B is blocked by A — must NOT appear.
    assert!(
        !ready_ids.contains(&task_b_id),
        "task B ({task_b_id}) is blocked and must not be in ready list; got: {ready_ids:?}"
    );
}

#[tokio::test]
async fn observe_ready_summaries_preserve_plan_labels() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan P1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("P1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let task_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    );
    let task_id = parse_id_from_create(&task_json);
    label_issue(dir.path(), &task_id, &labels::plan_id("P1"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::new(pm),
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let summaries = reconciler
        .observe_ready_summaries()
        .await
        .expect("ready summaries");
    assert!(summaries.iter().any(|ready| {
        ready.summary.id == task_id && ready.summary.labels.contains(&labels::plan_id("P1"))
    }));
}

#[tokio::test]
async fn pending_label_on_closed_epic_blocks_dispatch() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let complete_epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Complete Epic",
            "--priority",
            "2",
        ],
    ));
    let pending_epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Closed Pending Epic",
            "--priority",
            "2",
        ],
    ));

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &complete_epic_id, &plan_label);
    label_issue(dir.path(), &complete_epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &pending_epic_id, &plan_label);
    label_issue(dir.path(), &pending_epic_id, labels::PLAN_PENDING);
    run_br(
        dir.path(),
        &["update", &pending_epic_id, "--status", "closed"],
    );

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );
    let mut cache = std::collections::HashMap::new();

    let state = reconciler
        .plan_allows_dispatch("P1", &mut cache)
        .await
        .expect("plan_allows_dispatch");

    assert_eq!(
        state,
        PlanDispatchState::PlanHasPendingEpic {
            epic_id: pending_epic_id
        }
    );
}

#[tokio::test]
async fn plan_enumeration_finds_tasks_buried_under_backlog() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let mut backlog_jsonl = String::new();
    for idx in 0..1001 {
        let issue = serde_json::json!({
            "id": format!("bd-b{idx:04}"),
            "title": format!("High-priority backlog {idx:04}"),
            "status": "open",
            "priority": 1,
            "issue_type": "task",
            "created_at": "2026-04-30T00:00:00Z",
            "created_by": "test",
            "updated_at": "2026-04-30T00:00:00Z",
            "source_repo": ".",
            "compaction_level": 0,
            "original_size": 0,
        });
        backlog_jsonl.push_str(&serde_json::to_string(&issue).expect("serialize backlog issue"));
        backlog_jsonl.push('\n');
    }
    std::fs::write(dir.path().join(".beads/issues.jsonl"), backlog_jsonl)
        .expect("write backlog jsonl");
    run_br(dir.path(), &["sync", "--import-only"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let tasks = vec![plan_task("t1"), plan_task("t2")];
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "buried-plan",
        "Buried Plan",
        None,
        &tasks,
    )
    .await
    .expect("plan subgraph");
    let task_1_id = subgraph
        .task_map
        .get("t1")
        .expect("task map contains t1")
        .clone();
    let task_2_id = subgraph
        .task_map
        .get("t2")
        .expect("task map contains t2")
        .clone();

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        None,
        common::server_builder::pro_feature_gate(),
    );

    let summaries = reconciler
        .observe_ready_summaries()
        .await
        .expect("observe_ready_summaries");
    let ready_ids = summaries
        .iter()
        .map(|ready| ready.summary.id.as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        ready_ids.len(),
        2,
        "global reconciler should return only ready plan tasks; got: {summaries:?}"
    );
    assert!(
        ready_ids.contains(task_1_id.as_str()),
        "expected t1 ({task_1_id}) in ready summaries; got: {summaries:?}"
    );
    assert!(
        ready_ids.contains(task_2_id.as_str()),
        "expected t2 ({task_2_id}) in ready summaries; got: {summaries:?}"
    );
}

/// Exercises the br fallback path directly via `observe_ready_via_br`.
///
/// This test bypasses the bv primary path and calls the br fallback helper
/// directly. It verifies that task-level `br ready` candidates are filtered
/// through the plan epic's activation labels.
#[tokio::test]
async fn observe_ready_via_br_returns_ready_tasks() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // --- Create epic + 2 tasks ---
    let epic_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan P1 Epic (br fallback)",
            "--priority",
            "2",
        ],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A (unblocked, br path)",
            "--priority",
            "2",
        ],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B (blocked by A, br path)",
            "--priority",
            "2",
        ],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    // --- Task B depends on Task A (B is blocked by A) ---
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    // --- Label all three with plan-id:P1 ---
    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);

    // --- Label epic with spur:plan-complete (tasks do NOT get this label) ---
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    // --- Construct PmService ---
    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    // --- Build reconciler scoped to plan P1 ---
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    // --- Call the br fallback path directly (bypasses bv entirely) ---
    let ready_ids = reconciler
        .observe_ready_via_br()
        .await
        .expect("observe_ready_via_br must not fail");

    // Task A is unblocked — must appear in the br fallback result.
    assert!(
        ready_ids.contains(&task_a_id),
        "expected task A ({task_a_id}) in br fallback ready list; got: {ready_ids:?}"
    );

    // Task B is blocked by A — must NOT appear.
    assert!(
        !ready_ids.contains(&task_b_id),
        "task B ({task_b_id}) is blocked and must not be in br fallback ready list; got: {ready_ids:?}"
    );
}

#[tokio::test]
async fn observe_ready_via_br_suppresses_tasks_for_closed_complete_epic() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Closed Plan P1 Epic",
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
            "Task under closed complete epic",
            "--priority",
            "2",
        ],
    ));

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_id, &plan_label);
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close epic");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let ready_ids = reconciler
        .observe_ready_via_br()
        .await
        .expect("observe_ready_via_br");
    assert!(
        !ready_ids.contains(&task_id),
        "closed complete epic must not allow task dispatch through br fallback; got: {ready_ids:?}"
    );
}

#[tokio::test]
async fn epic_closes_when_scoped_children_terminal() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan terminal epic",
            "--priority",
            "2",
        ],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A terminal",
            "--priority",
            "2",
        ],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B terminal",
            "--priority",
            "2",
        ],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    pm.update_issue(
        &task_a_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task A");
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task B");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx: tokio::sync::mpsc::channel(1).0,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler
        .tick_once()
        .await
        .expect("tick_once must succeed");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());

    let epic_comments = run_br_json(dir.path(), &["comments", "list", &epic_id]);
    let sentinels = collect_sentinels(&epic_comments);
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion { plan_id, epic_id: found_epic_id, .. }
            if plan_id == "P1" && found_epic_id == &epic_id
    )));
}

#[tokio::test]
async fn all_approved_epic_emits_plan_ready_to_merge() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan ready epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A approved",
            "--priority",
            "2",
        ],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B approved",
            "--priority",
            "2",
        ],
    ));

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close task");
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
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: Some(sink_ref),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler
        .tick_once()
        .await
        .expect("tick_once must succeed");

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.body,
        SpurEventBody::PlanReadyToMerge { plan_id } if plan_id == "P1"
    )));
}

#[tokio::test]
async fn tick_once_persists_dispatch_before_queue_send() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(did_work);
    let request = delegation_rx.recv().await.expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn tick_once_persists_failed_completion_when_respond_to_drops() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let fast_forward = Arc::new(Notify::new());
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::clone(&fast_forward),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    assert!(reconciler.tick_once().await.expect("tick_once"));
    let request = delegation_rx.recv().await.expect("dispatch request");
    let delegation_id = request.id.as_str().to_string();
    label_issue(
        dir.path(),
        &task_id,
        &labels::lease_expires_at(4_102_444_800),
    );
    drop(request.respond_to);

    task_tracker.close();
    tokio::time::timeout(std::time::Duration::from_secs(60), task_tracker.wait())
        .await
        .expect("completion task should finish");
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        fast_forward.notified(),
    )
    .await
    .expect("completion should fast-forward the reconciler");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, "open");
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with(labels::LEASE_EXPIRES_AT_PREFIX)));

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "plan-1",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("project plan");
    let task = projected
        .tasks
        .iter()
        .find(|task| task.spec.issue_id.as_deref() == Some(task_id.as_str()))
        .expect("projected task");
    assert!(matches!(task.status, spur_mcp::plan::PlanTaskStatus::Ready));
    assert_eq!(task.attempt, 2);
    assert_eq!(task.history.len(), 1);

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &task_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            delegation_id: found_delegation_id,
            completion_state: audit_sentinel::CompletionState::Failed,
            result_summary,
            ..
        } if found_delegation_id == &delegation_id
            && result_summary.as_deref() == Some("orchestrator disconnected")
    )));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::RetryRequested {
            delegation_id: found_delegation_id,
            attempt: 1,
            error,
            ..
        } if found_delegation_id == &delegation_id
            && error == "orchestrator disconnected"
    )));
}

#[tokio::test]
async fn tick_once_reclaims_expired_lease_dispatch() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-lease"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Leased Task",
            "--priority",
            "2",
        ],
    ));
    let delegation_id = "del-expired";
    let expired_at = 1;
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-lease"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t-expired"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));
    label_issue(dir.path(), &task_id, &labels::delegation_id(delegation_id));
    label_issue(dir.path(), &task_id, &labels::lease_expires_at(expired_at));
    spur_mcp::plan::emit_dispatch_audit(
        Some(pm.as_ref()),
        &Some(task_id.clone()),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-lease",
        delegation_id,
        "codex",
        1,
    )
    .await;

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let event_sink: Arc<dyn McpEventSink> = sink.clone();
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: Some(event_sink),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-lease".into()),
        common::server_builder::pro_feature_gate(),
    );

    assert!(reconciler.tick_once().await.expect("tick_once"));
    let retry_request = delegation_rx
        .try_recv()
        .expect("expired lease reclamation should dispatch the auto-retry");
    assert_eq!(retry_request.issue_id.as_deref(), Some(task_id.as_str()));
    assert_ne!(
        retry_request.id.as_str(),
        delegation_id,
        "auto-retry dispatch must use a fresh delegation id"
    );

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, "open");
    assert!(issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));
    assert!(issue
        .labels
        .iter()
        .any(|label| label.starts_with(labels::LEASE_EXPIRES_AT_PREFIX)));

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &task_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            delegation_id: found_delegation_id,
            completion_state: audit_sentinel::CompletionState::Failed,
            result_summary,
            ..
        } if found_delegation_id == delegation_id
            && result_summary.as_deref() == Some("dispatch lease expired")
    )));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::RetryRequested {
            delegation_id: found_delegation_id,
            attempt: 1,
            error,
            ..
        } if found_delegation_id == delegation_id
            && error == "dispatch lease expired"
    )));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Dispatch {
            delegation_id: found_delegation_id,
            attempt: 2,
            ..
        } if found_delegation_id == retry_request.id.as_str()
    )));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::DispatchOrphanCleared {
            delegation_id: found_delegation_id,
            reason,
        } if found_delegation_id == delegation_id
            && reason.contains("dispatch lease expired")
            && reason.contains(&expired_at.to_string())
    )));

    let events = sink.events.lock().expect("events lock");
    assert!(events.iter().any(|event| matches!(
        &event.body,
        SpurEventBody::DispatchLeaseExpired {
            plan_id,
            task_id: event_task_id,
            issue_id,
            delegation_id: event_delegation_id,
            expired_at: event_expired_at,
            age_secs,
        } if plan_id == "plan-lease"
            && event_task_id == "t-expired"
            && issue_id == &task_id
            && event_delegation_id == delegation_id
            && *event_expired_at == expired_at
            && *age_secs >= 0
    )));
}

#[tokio::test]
async fn worker_success_after_orphan_clear_is_superseded() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let adv = pm.advanced().expect("advanced beads");

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Late Worker Completion",
            "--priority",
            "2",
        ],
    ));
    let delegation_id = "del-late-worker";
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-race"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t-late"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));
    label_issue(dir.path(), &task_id, &labels::delegation_id(delegation_id));
    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task");
    adv.add_comment(
        &task_id,
        &audit_sentinel::encode_comment(&AuditSentinelKind::DispatchOrphanCleared {
            delegation_id: delegation_id.to_string(),
            reason: "dispatch lease expired at 1 (age 1s)".to_string(),
        }),
    )
    .await
    .expect("add orphan audit");

    let result = spur_acp::DelegationResult {
        status: spur_acp::DelegationStatus::Success,
        diff: None,
        diff_summary: None,
        summary: Some("worker completed after reclaim".to_string()),
        estimated_cost_usd: 0.0,
        worker_branch: Some("spur/worker-late".to_string()),
        artifact: None,
    };
    let _deferred = spur_mcp::test_support::persist_worker_completion_and_notify(
        pm.as_ref(),
        &task_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-race",
        delegation_id,
        &None,
        &result,
        &BrainSessionId::new(SessionId("brain".into())),
        1,
        &test_materializer(),
        None,
    )
    .await
    .expect("persist worker completion");

    let audits = spur_mcp::plan::projector::collect_sorted_audits(
        adv.list_comments(&task_id).await.expect("list comments"),
    );
    let latest_completion = audits
        .iter()
        .rev()
        .find_map(|audit| match audit {
            AuditSentinelKind::Completion {
                delegation_id: found_delegation_id,
                completion_state,
                superseded,
                ..
            } if found_delegation_id == delegation_id => Some((completion_state, *superseded)),
            _ => None,
        })
        .expect("completion audit");
    assert_eq!(
        latest_completion.0,
        &audit_sentinel::CompletionState::Superseded
    );
    assert!(latest_completion.1);

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, pm.closed_status());
    assert!(!issue.labels.contains(&labels::READY_FOR_REVIEW.to_string()));
    assert!(!issue.labels.contains(&labels::delegation_id(delegation_id)));
}

#[tokio::test]
async fn tick_once_does_not_reclaim_live_lease() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-live"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Live Leased Task",
            "--priority",
            "2",
        ],
    ));
    let delegation_id = "del-live";
    let expires_at = 4_102_444_800;
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-live"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t-live"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));
    label_issue(dir.path(), &task_id, &labels::delegation_id(delegation_id));
    label_issue(dir.path(), &task_id, &labels::lease_expires_at(expires_at));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let event_sink: Arc<dyn McpEventSink> = sink.clone();
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: Some(event_sink),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-live".into()),
        common::server_builder::pro_feature_gate(),
    );

    assert!(!reconciler.tick_once().await.expect("tick_once"));
    assert!(delegation_rx.try_recv().is_err());

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, "open");
    assert!(issue.labels.contains(&labels::delegation_id(delegation_id)));
    assert!(issue.labels.contains(&labels::lease_expires_at(expires_at)));

    let events = sink.events.lock().expect("events lock");
    assert!(!events
        .iter()
        .any(|event| matches!(event.body, SpurEventBody::DispatchLeaseExpired { .. })));
}

#[tokio::test]
async fn tick_once_clears_dispatch_label_when_send_fails() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, delegation_rx) = tokio::sync::mpsc::channel(1);
    drop(delegation_rx);

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let _ = reconciler.tick_once().await;
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));
}

#[tokio::test]
async fn tick_once_skips_broken_plan_and_dispatches_other_ready_work() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);

    let broken_task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Broken Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &broken_task_id, &labels::plan_id("bogus-plan"));

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Valid Plan Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let valid_task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Valid Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &valid_task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &valid_task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &valid_task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        None,
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler
        .tick_once()
        .await
        .expect("tick_once should skip the broken plan and continue dispatching");
    assert!(did_work, "a valid ready task should still be dispatched");

    let request = delegation_rx.recv().await.expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(valid_task_id.as_str()));
}

#[tokio::test]
async fn resolve_dispatch_orphan_emits_breadcrumb_and_clears_label() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Dispatch Orphan",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::delegation_id("del-A"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let adv = pm.advanced().expect("advanced");

    let cleared = spur_mcp::server::resolve_dispatch_orphan(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("resolve dispatch orphan");
    assert!(cleared, "dispatch orphan should be cleared");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));

    let audits = adv
        .list_comments(&task_id)
        .await
        .expect("list comments")
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect::<Vec<_>>();
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::DispatchOrphanCleared {
            delegation_id,
            reason,
        } if delegation_id == "del-A"
            && reason == spur_mcp::test_support::ORPHAN_CLEAR_REASON_RESTART
    )));
}

#[tokio::test]
async fn resolve_dispatch_orphan_preserves_legacy_ready_for_review_marker() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Legacy Review Ready",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::delegation_id("del-legacy"));
    label_issue(dir.path(), &task_id, "ready-for-review");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);

    let cleared = spur_mcp::server::resolve_dispatch_orphan(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("resolve dispatch orphan");
    assert!(
        !cleared,
        "legacy ready-for-review marker should block dispatch orphan cleanup"
    );

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == &labels::delegation_id("del-legacy")),
        "delegation label must be preserved while task is awaiting review"
    );
}

#[tokio::test]
async fn execute_epic_persists_execution_scope_labels_on_epic_and_tasks() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Execute Epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
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
        dir.path(),
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
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&brain_sid.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("pre-seed current owner label");

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    eprintln!("execute_epic_response={response}");
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert!(
        epic.labels
            .iter()
            .any(|label| label == &labels::plan_owner(&brain_sid.as_session_id().0)),
        "execute_epic should stamp current owner label for current brain session"
    );
    let owner_labels: Vec<&str> = epic
        .labels
        .iter()
        .filter_map(|label| labels::parse_plan_owner(label))
        .collect();
    assert_eq!(
        owner_labels.len(),
        1,
        "execute_epic should replace old owner labels instead of accumulating them; got {owner_labels:?}"
    );
    assert_eq!(
        owner_labels[0],
        &brain_sid.as_session_id().0.replace('-', ""),
        "execute_epic should keep exactly current owner"
    );
    assert!(epic
        .labels
        .iter()
        .any(|label| label.starts_with("spur:plan-id:")));

    for task_id in [&task_a_id, &task_b_id] {
        let task = pm.get_issue(task_id).await.expect("get task");
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")));
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-task-id:")));
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:agent:")));
    }
}

#[tokio::test]
async fn execute_epic_rejects_persisted_non_terminal_epic_on_second_call() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Execute Epic Twice",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
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
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let first = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        first.get("error").is_none(),
        "first execute_epic should succeed: {first}"
    );
    let first_text = first["result"]["content"][0]["text"]
        .as_str()
        .expect("execute_epic response text");
    let first_json: serde_json::Value =
        serde_json::from_str(first_text).expect("execute_epic response JSON");
    let first_plan_id = first_json["plan_id"].as_str().expect("plan_id").to_string();

    server
        .__test_corrupt_cached_plan(
            &first_plan_id,
            &task_a_id,
            "spur/bogus-worker",
            "spur/bogus-snapshot",
        )
        .await
        .expect("corrupt cached plan");

    let second = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    let msg = second
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("already a persisted plan epic")
            && msg.contains(&first_plan_id)
            && msg.contains("use claim/start/resume plan instead"),
        "second execute_epic must reject persisted plan epics at the execute boundary: {second}"
    );
}

/// Regression for bd-19od: when a child task already carries the correct
/// `spur:agent:<name>` label, `execute_epic` must NOT strip it. The previous
/// implementation pushed the same string to both `add_labels` and
/// `remove_labels`; beads processes adds first, then removes, so the agent
/// label was wiped — causing the next dispatch tick to error with
/// `no agent for task; set spur:agent:<name>` and fall back to the hardcoded
/// `"codex"` default in projector.rs.
#[tokio::test]
async fn execute_epic_preserves_pre_existing_agent_label_on_child_task() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Preserve Agent Label",
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
            "Preserve Agent Label Child",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_id, &epic_id]);
    label_issue(dir.path(), &task_id, &labels::agent("claude-code"));

    let pre = run_br_json(dir.path(), &["show", &task_id]);
    assert!(
        pre.contains(&labels::agent("claude-code")),
        "precondition: child must carry spur:agent:claude-code before execute_epic; got {pre}"
    );

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "claude-code".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&epic_id, Some("claude-code"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let task = pm.get_issue(&task_id).await.expect("get task");
    let agent_label = labels::agent("claude-code");
    assert!(
        task.labels.iter().any(|l| l == &agent_label),
        "child task must retain its pre-existing agent label after execute_epic; got labels={:?}",
        task.labels
    );
}

#[tokio::test]
async fn execute_epic_rolls_back_epic_scope_when_task_scope_persist_fails() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Rollback Epic",
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
            "Rollback Task",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "bad/agent".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&epic_id, Some("bad/agent"))
        .await;
    assert!(
        response.get("error").is_some(),
        "execute_epic should fail when task scope label persistence is invalid: {response}"
    );

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")),
        "epic scope labels should roll back on task persist failure"
    );
    let task = pm.get_issue(&task_id).await.expect("get task");
    assert!(
        !task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")),
        "task should not retain partially-written plan scope after execute_epic failure"
    );
}

#[tokio::test]
async fn submit_plan_default_notify_path_dispatches_ready_task() {
    skip_if_no_loopback!("submit_plan_default_notify_path_dispatches_ready_task");

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);

    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
    server.__test_wait_startup_recovery().await;
    Arc::clone(&server)
        .enable_reconciler()
        .await
        .expect("enable reconciler");

    let response = server
        .__test_call_submit_plan(serde_json::json!({
            "persist_as_epic": true,
            "epic_title": "Default Notify Persisted Submit Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Dispatch via started reconciler",
                "depends_on": [],
            }]
        }))
        .await;
    let task_id = extract_submit_plan_task_issue_id(&response, "t1");

    let request = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        channel.request_rx.recv(),
    )
    .await
    .expect("started reconciler should dispatch persisted submit_plan work within timeout")
    .expect("dispatch request");

    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn execute_epic_default_notify_path_dispatches_ready_task() {
    skip_if_no_loopback!("execute_epic_default_notify_path_dispatches_ready_task");

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Default Notify Execute Epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
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
        dir.path(),
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
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);
    label_issue(dir.path(), &epic_id, &labels::plan_owner("brain"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
    server.__test_wait_startup_recovery().await;
    Arc::clone(&server)
        .enable_reconciler()
        .await
        .expect("enable reconciler");

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let request = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        channel.request_rx.recv(),
    )
    .await
    .expect("started reconciler should dispatch execute_epic work within timeout")
    .expect("dispatch request");

    assert_eq!(request.issue_id.as_deref(), Some(task_a_id.as_str()));
}

#[tokio::test]
async fn execute_epic_shutdown_abort_does_not_emit_plan_snapshot() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Shutdown Abort Execute Epic",
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
            "Task A",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(Arc::clone(&pm)),
        Some(sink_ref),
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    server.shutdown().await;

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert_eq!(
        response["error"]["message"].as_str(),
        Some("orchestrator shutting down — execute_epic aborted"),
        "execute_epic should abort cleanly when task tracker is closed: {response}"
    );
    assert_eq!(
        server.__test_active_plan_count().await,
        0,
        "shutdown abort should not leave a cached active plan behind"
    );

    let events = sink.events.lock().unwrap();
    let snapshot_count = events
        .iter()
        .filter(|event| matches!(event.body, SpurEventBody::PlanSnapshotUpdated { .. }))
        .count();
    assert_eq!(
        snapshot_count, 0,
        "shutdown abort must not emit a PlanSnapshotUpdated event for a rolled-back plan"
    );
}

/// D1 regression: `Reconciler::run` must honor cancel even while a tick is
/// mid-flight. Prior to the biased cancel-race select, cancel could only win
/// between ticks; a stuck `bv.triage`/`br ready` would hang shutdown.
///
/// Strategy: spin the reconciler at a very fast cadence so ticks are nearly
/// always in flight, let it run for a brief warm-up, then fire cancel. With
/// the fix in place the spawned task must complete within the 1-second
/// timeout budget.
#[tokio::test]
async fn reconciler_cancels_during_tick() {
    use std::time::Duration;

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // Seed a few issues so tick_once has non-trivial work to process.
    for idx in 0..5 {
        run_br_json(
            dir.path(),
            &[
                "create",
                "--type",
                "task",
                "--title",
                &format!("Warm-up task {idx}"),
                "--priority",
                "2",
            ],
        );
    }

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    // Very fast cadence → high probability that cancel lands mid-tick.
    let cfg = ReconcilerConfig {
        base_interval: Duration::from_millis(5),
        idle_ceiling: Duration::from_millis(50),
        backoff_factor: 2,
        ..Default::default()
    };
    let reconciler = Reconciler::new(
        cfg,
        pm,
        Arc::new(Notify::new()),
        None,
        None,
        common::server_builder::pro_feature_gate(),
    );

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move { reconciler.run(cancel_rx).await });

    // Let the run loop fire several ticks.
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancel_tx.send(()).expect("cancel receiver alive");

    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("reconciler must shut down within 1s of cancel")
        .expect("reconciler task must not panic");
}

/// Hybrid wake equivalence: a fast-forward signal causes the reconciler to tick
/// and produce the same observable dispatch as a direct `tick_once()` call.
/// The journal wake is optional and does not change `tick_once()` semantics.
#[tokio::test]
async fn hybrid_fast_forward_matches_polling_projection() {
    use std::time::Duration;

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);
    let brain_sid = BrainSessionId::new(SessionId::new());
    label_issue(
        dir.path(),
        &epic_id,
        &labels::plan_owner(&brain_sid.as_session_id().0),
    );

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let fast_forward = Arc::new(Notify::new());

    let reconciler = Reconciler::new(
        ReconcilerConfig {
            base_interval: Duration::from_secs(60),
            idle_ceiling: Duration::from_secs(60),
            backoff_factor: 2,
            ..Default::default()
        },
        Arc::clone(&pm),
        Arc::clone(&fast_forward),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: brain_sid,
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    // Spawn run() with a long interval; without fast-forward it would not tick for 60s.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move { reconciler.run(cancel_rx).await });

    // Yield briefly to let the reconciler enter the select! loop.
    tokio::task::yield_now().await;

    // Fast-forward should trigger an immediate tick and dispatch the ready task.
    // Re-notify within a bounded window so the test is not sensitive to whether
    // the first wake lands before the run loop enters its select.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut request = None;
    while tokio::time::Instant::now() < deadline {
        fast_forward.notify_one();
        match tokio::time::timeout(Duration::from_millis(100), delegation_rx.recv()).await {
            Ok(Some(req)) => {
                request = Some(req);
                break;
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    assert!(
        request.is_some(),
        "fast-forward must trigger tick and dispatch within timeout"
    );
    let request = request.expect("fast-forward must produce a delegation request");
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));

    // Cancel and clean up.
    let _ = cancel_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
