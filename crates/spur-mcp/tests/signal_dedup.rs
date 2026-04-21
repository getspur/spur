//! T-F7: duplicate worker signals with the same `signal_id` are applied once.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::mutation::MutationBatch;
use spur_mcp::plan::proposers::{MutationProposer, ScopeDriftSplitProposer, TrivialScorer};
use spur_mcp::plan::signal_watcher::SignalWatcher;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_mcp::plan::{PlanState, PlanTask};
use spur_pm::PmService;
use tempfile::TempDir;
use uuid::Uuid;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(
            None,  // no github_repo
            true,  // beads_enabled
            false, // github_enabled
            repo, None, // closed_status default
        )
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init"),
    )
}

fn scope_drift_signal(signal_id: Uuid) -> WorkerSignal {
    WorkerSignal::ScopeDrift {
        signal_id,
        severity: 0.82,
        reason: "auth refactor pulls in 4 new subsystems".into(),
        estimated_subtasks: Some(3),
    }
}

async fn add_labels_individually(pm: &PmService, issue_id: &str, labels: &[String]) {
    for label in labels {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .expect("seed label");
    }
}

async fn build_single_task_plan(pm: &PmService, plan_id: &str) -> String {
    let tasks = vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Signal watcher task".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }];
    let subgraph = spur_mcp::build_epic_subgraph(pm, plan_id, "Signal Watcher Epic", None, &tasks)
        .await
        .expect("build_epic_subgraph must succeed");
    subgraph
        .task_map
        .get("t1")
        .expect("t1 must exist in task_map")
        .clone()
}

struct ProjectedPlanSplitProposer {
    expected_plan_id: String,
}

#[async_trait]
impl MutationProposer for ProjectedPlanSplitProposer {
    async fn propose(
        &self,
        state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        if state.plan_id != self.expected_plan_id || state.tasks.is_empty() {
            return Vec::new();
        }

        ScopeDriftSplitProposer::default()
            .propose(state, signal, triggering_task)
            .await
    }
}

fn audit_sentinels(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

#[tokio::test]
async fn duplicate_signal_comments_with_same_signal_id_commit_once() {
    if !br_available() {
        eprintln!(
            "skipping duplicate_signal_comments_with_same_signal_id_commit_once: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-dedup-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_comment = signals::encode_comment(&signal);
    let advanced = pm.advanced().expect("advanced beads surface");
    advanced
        .add_comment(&task_id, &signal_comment)
        .await
        .expect("first signal comment");
    advanced
        .add_comment(&task_id, &signal_comment)
        .await
        .expect("second signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
    );
    watcher
        .tick_once()
        .await
        .expect("signal watcher tick must succeed");

    let comments = advanced
        .list_comments(&task_id)
        .await
        .expect("list comments after signal watcher tick");
    let audits = audit_sentinels(&comments);
    let mutation_commit_count = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::MutationCommit { .. }))
        .count();

    assert_eq!(
        mutation_commit_count, 1,
        "duplicate signal_id must produce exactly one MutationCommit sentinel; audits={audits:?}"
    );
}

#[tokio::test]
async fn watcher_skips_signal_task_without_ready_for_review_label() {
    if !br_available() {
        eprintln!(
            "skipping watcher_skips_signal_task_without_ready_for_review_label: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-gate-1").await;
    add_labels_individually(pm.as_ref(), &task_id, &[labels::signal_kind("scope-drift")]).await;

    let signal = scope_drift_signal(Uuid::new_v4());
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
    );
    watcher.tick_once().await.expect("tick_once");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(
        !issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:signal-processed:")),
        "task without spur:ready-for-review must remain unprocessed"
    );
}

#[tokio::test]
async fn watcher_projects_real_plan_state_for_scoring() {
    if !br_available() {
        eprintln!("skipping watcher_projects_real_plan_state_for_scoring: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-projector-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let signal = scope_drift_signal(Uuid::new_v4());
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ProjectedPlanSplitProposer {
            expected_plan_id: "signal-projector-1".into(),
        },
        TrivialScorer,
    );
    watcher.tick_once().await.expect("tick_once");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:signal-processed:")),
        "watcher must use projected persisted plan state to drive scoring"
    );
}

#[tokio::test]
async fn watcher_processes_only_one_signal_per_task_per_tick() {
    if !br_available() {
        eprintln!("skipping watcher_processes_only_one_signal_per_task_per_tick: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-one-per-tick-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let advanced = pm.advanced().expect("advanced beads surface");
    advanced
        .add_comment(
            &task_id,
            &signals::encode_comment(&scope_drift_signal(Uuid::new_v4())),
        )
        .await
        .expect("first signal comment");
    advanced
        .add_comment(
            &task_id,
            &signals::encode_comment(&scope_drift_signal(Uuid::new_v4())),
        )
        .await
        .expect("second signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ProjectedPlanSplitProposer {
            expected_plan_id: "signal-one-per-tick-1".into(),
        },
        TrivialScorer,
    );
    watcher.tick_once().await.expect("tick_once");

    let comments = advanced
        .list_comments(&task_id)
        .await
        .expect("list comments after watcher tick");
    let audits = audit_sentinels(&comments);
    let mutation_plans = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::MutationPlan { .. }))
        .count();
    assert_eq!(
        mutation_plans, 1,
        "watcher must commit at most one signal decision per task per tick; audits={audits:?}"
    );
}

#[tokio::test]
async fn watcher_skips_review_rejected_tasks_even_if_signal_label_exists() {
    if !br_available() {
        eprintln!(
            "skipping watcher_skips_review_rejected_tasks_even_if_signal_label_exists: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-rejected-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::REVIEW_REJECTED.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let signal = scope_drift_signal(Uuid::new_v4());
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
    );
    watcher.tick_once().await.expect("tick_once");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(
        !issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:signal-processed:")),
        "rejected tasks must stay watcher-ineligible even when signal labels exist"
    );
}
