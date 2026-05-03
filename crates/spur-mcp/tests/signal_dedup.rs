//! T-F7: duplicate worker signals with the same `signal_id` are applied once.

use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::mutation::{MutationBatch, TaskDraft};
use spur_mcp::plan::proposers::{MutationProposer, ScopeDriftSplitProposer, TrivialScorer};
use spur_mcp::plan::signal_watcher::SignalWatcher;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_mcp::plan::{PlanState, PlanTask};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::time::sleep;
use uuid::Uuid;

mod common;

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

fn sqlite_available() -> bool {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_sql(repo: &Path, sql: &str) -> Result<(), String> {
    let db = repo.join(".beads/beads.db");
    let output = Command::new("sqlite3")
        .arg("-cmd")
        .arg(".timeout 2000")
        .arg(db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "sqlite3 failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

fn run_sql_json(repo: &Path, sql: &str) -> Result<String, String> {
    let db = repo.join(".beads/beads.db");
    let output = Command::new("sqlite3")
        .arg("-cmd")
        .arg(".timeout 2000")
        .arg("-json")
        .arg(db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "sqlite3 -json failed (exit {}): stderr={stderr} stdout={stdout}",
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
    let subgraph = spur_mcp::build_epic_subgraph(
        pm,
        common::server_builder::pro_feature_gate().as_ref(),
        plan_id,
        "Signal Watcher Epic",
        None,
        &tasks,
    )
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

struct FixedMutationIdProposer {
    mutation_ids: Mutex<VecDeque<Uuid>>,
}

impl FixedMutationIdProposer {
    fn new(mutation_ids: Vec<Uuid>) -> Self {
        Self {
            mutation_ids: Mutex::new(mutation_ids.into()),
        }
    }
}

#[async_trait]
impl MutationProposer for FixedMutationIdProposer {
    async fn propose(
        &self,
        _state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        let Some(mutation_id) = self.mutation_ids.lock().pop_front() else {
            return Vec::new();
        };

        match signal {
            WorkerSignal::ScopeDrift {
                signal_id,
                reason,
                estimated_subtasks,
                ..
            } => {
                let child_count = estimated_subtasks.unwrap_or(2).max(2) as usize;
                let children = (0..child_count)
                    .map(|index| {
                        serde_json::from_value::<TaskDraft>(json!({
                            "title": format!("[retry {}/{}] {}", index + 1, child_count, reason),
                            "description": format!(
                                "Deterministic retry split for signal {} on task {}.",
                                signal_id, triggering_task
                            ),
                            "assignee": null,
                            "priority": null
                        }))
                        .expect("TaskDraft JSON must deserialize")
                    })
                    .collect::<Vec<_>>();
                vec![serde_json::from_value::<MutationBatch>(json!({
                    "mutation_id": mutation_id,
                    "trigger_signal_id": signal_id,
                    "trigger_task_id": triggering_task,
                    "ops": [{
                        "op": "split_task",
                        "parent": triggering_task,
                        "children": children,
                        "dep_rewire": {
                            "policy": "barrier"
                        }
                    }]
                }))
                .expect("MutationBatch JSON must deserialize")]
            }
            _ => Vec::new(),
        }
    }
}

fn audit_sentinels(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

fn issue_ids_for_label(repo: &Path, label: &str) -> Result<Vec<String>, String> {
    let rows = run_sql_json(
        repo,
        &format!(
            "SELECT issue_id FROM labels WHERE label = '{}' ORDER BY issue_id;",
            label
        ),
    )?;
    if rows.trim().is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::from_str::<Vec<Value>>(&rows)
        .map_err(|err| format!("parse sqlite label rows: {err}; raw={rows}"))?
        .into_iter()
        .filter_map(|row| {
            row.get("issue_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    Ok(ids)
}

async fn inject_cycle_when_children_exist(repo: PathBuf, mutation_id: Uuid) -> Result<(), String> {
    let label = labels::mutation_id_label(&mutation_id);
    for _ in 0..2_000 {
        let mut ids = issue_ids_for_label(&repo, &label)?;
        if ids.len() >= 2 {
            ids.sort();
            let sql = format!(
                "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by) \
                 VALUES ('{}', '{}', 'blocks', 'signal-dedup-test'); \
                 INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by) \
                 VALUES ('{}', '{}', 'blocks', 'signal-dedup-test');",
                ids[0], ids[1], ids[1], ids[0]
            );
            run_sql(&repo, &sql)?;
            return Ok(());
        }
        sleep(Duration::from_millis(2)).await;
    }
    Err("timed out waiting for mutation children before injecting cycle".into())
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn duplicate_signal_comments_with_same_signal_id_commit_once() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
        common::server_builder::pro_feature_gate(),
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn watcher_skips_signal_task_without_ready_for_review_label() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
        common::server_builder::pro_feature_gate(),
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn watcher_projects_real_plan_state_for_scoring() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
        common::server_builder::pro_feature_gate(),
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn watcher_processes_only_one_signal_per_task_per_tick() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
        common::server_builder::pro_feature_gate(),
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn watcher_skips_review_rejected_tasks_even_if_signal_label_exists() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
        common::server_builder::pro_feature_gate(),
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn watcher_retries_signal_after_invariant_violation_without_marking_processed() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    assert!(
        sqlite_available(),
        "this test requires `sqlite3` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-retry-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let signal = WorkerSignal::ScopeDrift {
        signal_id: Uuid::new_v4(),
        severity: 0.91,
        reason: "retry mutation after rollback".into(),
        estimated_subtasks: Some(2),
    };
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let first_mutation_id = Uuid::new_v4();
    let second_mutation_id = Uuid::new_v4();
    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        FixedMutationIdProposer::new(vec![first_mutation_id, second_mutation_id]),
        TrivialScorer,
        common::server_builder::pro_feature_gate(),
    );

    let first_injector = tokio::spawn(inject_cycle_when_children_exist(
        dir.path().to_path_buf(),
        first_mutation_id,
    ));
    watcher.tick_once().await.expect("first tick_once");
    first_injector
        .await
        .expect("first injector task panicked")
        .expect("first cycle injector failed");

    let first_issue = pm
        .get_issue(&task_id)
        .await
        .expect("load task after first tick");
    assert!(
        !first_issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:signal-processed:")),
        "invariant-violation rollback must not mark the signal processed"
    );

    let second_injector = tokio::spawn(inject_cycle_when_children_exist(
        dir.path().to_path_buf(),
        second_mutation_id,
    ));
    watcher.tick_once().await.expect("second tick_once");
    second_injector
        .await
        .expect("second injector task panicked")
        .expect("second cycle injector failed");

    let issue = pm.get_issue(&task_id).await.expect("load task after retry");
    assert!(
        !issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:signal-processed:")),
        "failed mutation retries must stay eligible until a commit succeeds"
    );

    let comments = pm
        .advanced()
        .expect("advanced beads surface")
        .list_comments(&task_id)
        .await
        .expect("list comments after retries");
    let audits = audit_sentinels(&comments);
    let mutation_plan_count = audits
        .iter()
        .filter(|audit| matches!(audit, AuditSentinelKind::MutationPlan { .. }))
        .count();
    let violation_count = audits
        .iter()
        .filter(|audit| matches!(audit, AuditSentinelKind::MutationInvariantViolation { .. }))
        .count();

    assert_eq!(
        mutation_plan_count, 2,
        "watcher must retry the same signal on a later tick after invariant violation: {audits:?}"
    );
    assert_eq!(
        violation_count, 2,
        "each failed retry should leave an invariant-violation breadcrumb: {audits:?}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn distinct_signal_on_task_with_prior_processed_label_is_not_skipped() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-exact-dedup-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;

    let old_signal_id = Uuid::new_v4();
    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::signal_processed_label(&old_signal_id)],
            ..Default::default()
        },
    )
    .await
    .expect("add old processed label");

    let new_signal = scope_drift_signal(Uuid::new_v4());
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&new_signal))
        .await
        .expect("new signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
        common::server_builder::pro_feature_gate(),
    );
    watcher.tick_once().await.expect("tick_once");

    let comments = pm
        .advanced()
        .expect("advanced beads surface")
        .list_comments(&task_id)
        .await
        .expect("list comments after watcher tick");
    let audits = audit_sentinels(&comments);
    let mutation_plan_count = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::MutationPlan { .. }))
        .count();

    assert_eq!(
        mutation_plan_count, 1,
        "a distinct signal must be processed even when the task already carries a different signal-processed label; audits={audits:?}"
    );
}
