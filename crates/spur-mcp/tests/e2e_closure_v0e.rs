//! Normalized v0e acceptance coverage for the opt-in auto-merge/PR path.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{AuditSentinelKind, EpicCompletionOutcome};
use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{
    Reconciler, ReconcilerAutomation, ReconcilerConfig, ReconcilerDispatchCtx,
};
use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
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
        .filter_map(spur_mcp::plan::audit_sentinel::parse_comment)
        .filter_map(|result| result.ok())
        .collect()
}

async fn beads_pm(repo: &Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

async fn seed_all_approved_epic(
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
            "Auto-Merge Epic",
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

struct RecordingAutomation {
    actions: Arc<tokio::sync::Mutex<Vec<String>>>,
    params: Arc<tokio::sync::Mutex<Vec<spur_pm::PrParams>>>,
}

#[async_trait::async_trait]
impl ReconcilerAutomation for RecordingAutomation {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<spur_mcp::plan::PlanMergeState> {
        self.actions.lock().await.push(format!("merge:{plan_id}"));
        Ok(spur_mcp::plan::PlanMergeState::Succeeded {
            merge_branch: "spur/merge-1".to_string(),
            merged_task_ids: vec!["task-a".to_string(), "task-b".to_string()],
        })
    }

    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.actions
            .lock()
            .await
            .push(format!("pr:{}", params.title));
        self.params.lock().await.push(params);
        Ok("https://example.invalid/pr/42".to_string())
    }
}

#[tokio::test]
async fn t_v0e_2_auto_merge_pr_is_opt_in() {
    if !br_available() {
        eprintln!("skipping t_v0e_2_auto_merge_pr_is_opt_in: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let (pm, epic_id, task_a_id, task_b_id) = seed_all_approved_epic(dir.path(), "P1").await;

    // Close both tasks so the epic becomes all-approved.
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

    let actions = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let params = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let automation = Arc::new(RecordingAutomation {
        actions: Arc::clone(&actions),
        params: Arc::clone(&params),
    });

    // --- Phase 1: config=false => zero automation calls ---
    {
        let mut reconciler = Reconciler::new(
            ReconcilerConfig::default(),
            Arc::clone(&pm),
            Arc::new(Notify::new()),
            Some(test_dispatch_ctx()),
            Some("P1".into()),
            common::server_builder::pro_feature_gate(),
        );
        reconciler.set_auto_merge_approved_plans(false);
        reconciler.set_automation(automation.clone());

        // First tick: close the epic and add integration-pending.
        reconciler.tick_once().await.expect("tick_once");

        let epic = pm.get_issue(&epic_id).await.expect("get epic");
        assert_eq!(epic.status, pm.closed_status());
        assert!(
            epic.labels.iter().any(|l| l == labels::INTEGRATION_PENDING),
            "epic must have integration-pending"
        );

        let recorded = actions.lock().await;
        assert!(
            recorded.is_empty(),
            "config-off must produce zero automation actions, got: {:?}",
            *recorded
        );
    }

    // --- Phase 2: config=true => exactly one merge + one PR ---
    {
        actions.lock().await.clear();

        let mut reconciler = Reconciler::new(
            ReconcilerConfig::default(),
            Arc::clone(&pm),
            Arc::new(Notify::new()),
            Some(test_dispatch_ctx()),
            Some("P1".into()),
            common::server_builder::pro_feature_gate(),
        );
        reconciler.set_auto_merge_approved_plans(true);
        reconciler.set_automation(automation.clone());

        // Second tick: epic is now closed with integration-pending -> automation fires.
        reconciler.tick_once().await.expect("tick_once");

        let recorded = actions.lock().await;
        let merge_calls: Vec<_> = recorded
            .iter()
            .filter(|a| a.starts_with("merge:"))
            .collect();
        let pr_calls: Vec<_> = recorded.iter().filter(|a| a.starts_with("pr:")).collect();
        assert_eq!(
            merge_calls.len(),
            1,
            "expected exactly one merge call, got: {:?}",
            *recorded
        );
        assert_eq!(
            pr_calls.len(),
            1,
            "expected exactly one PR call, got: {:?}",
            *recorded
        );

        let pr_params = params.lock().await;
        let pr = pr_params.first().expect("PR params recorded");
        assert!(
            pr.title.contains("P1"),
            "PR title must contain plan_id: {}",
            pr.title
        );
        assert!(
            pr.body.contains("All approved"),
            "PR body must contain outcome summary: {}",
            pr.body
        );
        assert_eq!(pr.head_branch, "spur/merge-1");
    }

    // --- Phase 3: idempotency — second tick must not duplicate automation ---
    {
        actions.lock().await.clear();
        params.lock().await.clear();

        let mut reconciler = Reconciler::new(
            ReconcilerConfig::default(),
            Arc::clone(&pm),
            Arc::new(Notify::new()),
            Some(test_dispatch_ctx()),
            Some("P1".into()),
            common::server_builder::pro_feature_gate(),
        );
        reconciler.set_auto_merge_approved_plans(true);
        reconciler.set_automation(automation.clone());

        reconciler.tick_once().await.expect("tick_once");

        let recorded = actions.lock().await;
        assert!(
            recorded.is_empty(),
            "second tick must not duplicate automation actions, got: {:?}",
            *recorded
        );
    }

    // Verify durable audit was emitted.
    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &epic_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::AllApproved,
            plan_id,
            epic_id: found_epic_id,
        } if plan_id == "P1" && found_epic_id == &epic_id
    )));
}

// ── Helpers for t_v0e_1 and t_v0e_3 ─────────────────────────────────────

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn test_dispatch_ctx() -> ReconcilerDispatchCtx {
    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);
    ReconcilerDispatchCtx {
        delegation_tx,
        task_tracker: TaskTracker::new(),
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        event_sink: None,
        materializer: test_materializer(),
    }
}

fn seed_ready_task(repo: &Path, plan_id: &str) -> (String, String) {
    let epic_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Wake Epic",
            "--priority",
            "2",
        ],
    ));
    let task_id = parse_id_from_create(&run_br_json(
        repo,
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
    label_issue(repo, &epic_id, &labels::plan_id(plan_id));
    label_issue(repo, &epic_id, &labels::plan_owner("brain"));
    label_issue(repo, &task_id, &labels::plan_id(plan_id));
    label_issue(repo, &task_id, &labels::plan_task_id("t1"));
    label_issue(repo, &task_id, &labels::agent("codex"));
    label_issue(repo, &epic_id, labels::PLAN_COMPLETE);
    (epic_id, task_id)
}

// ── T-v0e-1: persisted direct-dispatch retirement ───────────────────────

#[tokio::test]
async fn t_v0e_1_no_persisted_direct_dispatch() {
    if !br_available() {
        eprintln!("skipping t_v0e_1_no_persisted_direct_dispatch: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        &session_id,
        Some(pm),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    // 1. submit_plan(persist_as_epic=true) must not dispatch directly
    let response = server
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "Persisted Dispatch Retirement Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Do something",
                "depends_on": [],
            }]
        }))
        .await;
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );

    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        channel.request_rx.recv(),
    )
    .await;
    assert!(
        recv.is_err(),
        "persisted submit_plan must not dispatch directly"
    );

    // 2. execute_epic must not dispatch directly
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Exec Epic",
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
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);
    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        channel.request_rx.recv(),
    )
    .await;
    assert!(recv.is_err(), "execute_epic must not dispatch directly");

    // 3. persisted review approve must not dispatch directly
    let state = PlanState {
        plan_id: "p-review".into(),
        brain_session_id: session_id.clone(),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: Some(epic_id.clone()),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t-review".into(),
                agent: "codex".into(),
                task: "Review task".into(),
                depends_on: vec![],
                issue_id: Some(task_a_id.clone()),
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    };
    let plan_arc = Arc::new(tokio::sync::Mutex::new(state));
    let (dtx, mut drx) = tokio::sync::mpsc::channel(1);
    let _ = spur_mcp::plan::handle_review_task(
        plan_arc,
        "p-review",
        "t-review",
        "approve",
        None,
        None,
        None,
        Some(&dtx),
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("review_task approve should succeed");
    assert!(
        matches!(
            drx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "persisted review approve must not enqueue a follow-up dispatch"
    );
}

// ── T-v0e-3: wakeup equivalence ─────────────────────────────────────────

#[tokio::test]
async fn t_v0e_3_fast_forward_matches_polling() {
    if !br_available() {
        eprintln!("skipping t_v0e_3_fast_forward_matches_polling: `br` not on PATH");
        return;
    }

    let plan_id = "P-wake";

    // ── 1. Ready-task progression equivalence ─────────────────────────
    let dir_poll = TempDir::new().expect("tempdir");
    run_br(dir_poll.path(), &["init"]);
    let (_epic_poll, task_poll) = seed_ready_task(dir_poll.path(), plan_id);
    let pm_poll = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir_poll.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let (poll_tx, mut poll_rx) = tokio::sync::mpsc::channel(1);
    let poll_reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm_poll),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx: poll_tx,
            task_tracker: TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );
    let poll_did_work = poll_reconciler.tick_once().await.expect("poll tick");
    assert!(poll_did_work, "polling tick must observe ready task");
    let poll_req = poll_rx.recv().await;
    assert!(poll_req.is_some(), "polling tick must dispatch ready task");
    assert_eq!(
        poll_req.unwrap().issue_id.as_deref(),
        Some(task_poll.as_str())
    );

    let dir_ff = TempDir::new().expect("tempdir");
    run_br(dir_ff.path(), &["init"]);
    let (_epic_ff, task_ff) = seed_ready_task(dir_ff.path(), plan_id);
    let pm_ff = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir_ff.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let (ff_tx, mut ff_rx) = tokio::sync::mpsc::channel(1);
    let fast_forward = Arc::new(Notify::new());
    let ff_reconciler = Reconciler::new(
        ReconcilerConfig {
            base_interval: std::time::Duration::from_secs(60),
            idle_ceiling: std::time::Duration::from_secs(60),
            backoff_factor: 2,
            ..Default::default()
        },
        Arc::clone(&pm_ff),
        Arc::clone(&fast_forward),
        Some(ReconcilerDispatchCtx {
            delegation_tx: ff_tx,
            task_tracker: TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move { ff_reconciler.run(cancel_rx).await });
    tokio::task::yield_now().await;
    fast_forward.notify_one();
    let ff_req = tokio::time::timeout(std::time::Duration::from_secs(2), ff_rx.recv()).await;
    assert!(
        ff_req.is_ok(),
        "fast-forward must trigger dispatch within timeout"
    );
    let ff_req = ff_req.unwrap();
    assert!(
        ff_req.is_some(),
        "fast-forward must produce a delegation request"
    );
    assert_eq!(ff_req.unwrap().issue_id.as_deref(), Some(task_ff.as_str()));
    let _ = cancel_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    // ── 2. Terminal outcome equivalence ───────────────────────────────
    let dir_term_poll = TempDir::new().expect("tempdir");
    run_br(dir_term_poll.path(), &["init"]);
    let (pm_term_poll, epic_term_poll, task_a_poll, task_b_poll) =
        seed_all_approved_epic(dir_term_poll.path(), plan_id).await;
    for task_id in [&task_a_poll, &task_b_poll] {
        pm_term_poll
            .update_issue(
                task_id,
                spur_pm::IssueUpdate {
                    status: Some(pm_term_poll.closed_status().to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("close task");
    }
    let term_poll_reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm_term_poll),
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx()),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );
    let term_poll_did_work = term_poll_reconciler
        .tick_once()
        .await
        .expect("term poll tick");
    assert!(term_poll_did_work, "polling tick must close epic");
    let epic = pm_term_poll
        .get_issue(&epic_term_poll)
        .await
        .expect("get epic");
    assert_eq!(epic.status, pm_term_poll.closed_status());
    assert!(
        epic.labels.iter().any(|l| l == labels::INTEGRATION_PENDING),
        "polling path must add integration-pending"
    );

    let dir_term_ff = TempDir::new().expect("tempdir");
    run_br(dir_term_ff.path(), &["init"]);
    let (pm_term_ff, epic_term_ff, task_a_ff, task_b_ff) =
        seed_all_approved_epic(dir_term_ff.path(), plan_id).await;
    for task_id in [&task_a_ff, &task_b_ff] {
        pm_term_ff
            .update_issue(
                task_id,
                spur_pm::IssueUpdate {
                    status: Some(pm_term_ff.closed_status().to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("close task");
    }
    let term_fast_forward = Arc::new(Notify::new());
    let term_ff_reconciler = Reconciler::new(
        ReconcilerConfig {
            base_interval: std::time::Duration::from_secs(60),
            idle_ceiling: std::time::Duration::from_secs(60),
            backoff_factor: 2,
            ..Default::default()
        },
        Arc::clone(&pm_term_ff),
        Arc::clone(&term_fast_forward),
        Some(test_dispatch_ctx()),
        Some(plan_id.into()),
        common::server_builder::pro_feature_gate(),
    );
    let (cancel_tx2, cancel_rx2) = tokio::sync::oneshot::channel();
    let handle2 = tokio::spawn(async move { term_ff_reconciler.run(cancel_rx2).await });
    tokio::task::yield_now().await;
    term_fast_forward.notify_one();
    let epic = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let epic = pm_term_ff.get_issue(&epic_term_ff).await.expect("get epic");
            if epic.labels.iter().any(|l| l == labels::INTEGRATION_PENDING) {
                break epic;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("fast-forward path must add integration-pending within timeout");
    assert_eq!(epic.status, pm_term_ff.closed_status());
    assert!(
        epic.labels.iter().any(|l| l == labels::INTEGRATION_PENDING),
        "fast-forward path must add integration-pending"
    );
    let _ = cancel_tx2.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle2).await;
}
