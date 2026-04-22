//! Normalized v0e acceptance coverage for the opt-in auto-merge/PR path.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_mcp::plan::audit_sentinel::{AuditSentinelKind, EpicCompletionOutcome};
use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerAutomation, ReconcilerConfig};
use tempfile::TempDir;
use tokio::sync::Notify;

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
        panic!("br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}", output.status);
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
        panic!("br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}", output.status);
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
        &["create", "--type", "epic", "--title", "Auto-Merge Epic", "--priority", "2"],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        repo,
        &["create", "--type", "task", "--title", "Task A", "--priority", "2"],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        repo,
        &["create", "--type", "task", "--title", "Task B", "--priority", "2"],
    ));

    let plan_label = labels::plan_id(plan_id);
    for issue_id in [&epic_id, &task_a_id, &task_b_id] {
        label_issue(repo, issue_id, &plan_label);
    }
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
        self.actions.lock().await.push(format!("pr:{}", params.title));
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
            None,
            Some("P1".into()),
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
            None,
            Some("P1".into()),
        );
        reconciler.set_auto_merge_approved_plans(true);
        reconciler.set_automation(automation.clone());

        // Second tick: epic is now closed with integration-pending -> automation fires.
        reconciler.tick_once().await.expect("tick_once");

        let recorded = actions.lock().await;
        let merge_calls: Vec<_> = recorded.iter().filter(|a| a.starts_with("merge:")).collect();
        let pr_calls: Vec<_> = recorded.iter().filter(|a| a.starts_with("pr:")).collect();
        assert_eq!(
            merge_calls.len(),
            1,
            "expected exactly one merge call, got: {:?}",
            *recorded
        );
        assert_eq!(pr_calls.len(), 1, "expected exactly one PR call, got: {:?}", *recorded);

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
            None,
            Some("P1".into()),
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
