//! T-F7: duplicate worker signals with the same `signal_id` are applied once.
#![allow(clippy::await_holding_lock)]

use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::{params, Error as SqliteError, ErrorCode};
use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::mutation::{MutationBatch, TaskDraft};
use spur_mcp::plan::mutation_executor::SIGNAL_ESCALATED_LABEL;
use spur_mcp::plan::proposers::{
    MutationProposer, MutationScorer, RetryExhaustedProposer, ScopeDriftSplitProposer,
    TrivialScorer,
};
use spur_mcp::plan::signal_watcher::SignalWatcher;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_mcp::plan::{PlanState, PlanTask};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::sleep;
use uuid::Uuid;

mod common;

static SIGNAL_TEST_MUTEX: OnceLock<Arc<AsyncMutex<()>>> = OnceLock::new();

async fn signal_test_guard() -> OwnedMutexGuard<()> {
    Arc::clone(SIGNAL_TEST_MUTEX.get_or_init(|| Arc::new(AsyncMutex::new(()))))
        .lock_owned()
        .await
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
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

fn retry_exhausted_signal(signal_id: Uuid, task_id: &str) -> WorkerSignal {
    WorkerSignal::RetryExhausted {
        signal_id,
        task_id: task_id.to_string(),
        attempt: 1,
        last_error: "retry budget exhausted".into(),
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
        issue_title: None,
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

async fn seed_completion_with_base_oid(pm: &PmService, task_id: &str) {
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(
            task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            }),
        )
        .await
        .expect("seed dispatch audit");
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(
            task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: "del-1".into(),
                completion_state: audit_sentinel::CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-t1".into()),
                result_summary: Some("ready for review".into()),
                artifact_uri: None,
                dispatched_base_oid: Some("0000000000000000000000000000000000000001".into()),
            }),
        )
        .await
        .expect("seed completion audit");
}

struct ProjectedPlanRetryProposer {
    expected_plan_id: String,
}

#[async_trait]
impl MutationProposer for ProjectedPlanRetryProposer {
    async fn propose(
        &self,
        state: &PlanState,
        signal: &WorkerSignal,
        triggering_task: &str,
    ) -> Vec<MutationBatch> {
        if state.plan_id != self.expected_plan_id || state.tasks.is_empty() {
            return Vec::new();
        }

        RetryExhaustedProposer
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
            WorkerSignal::RetryExhausted {
                signal_id,
                last_error,
                ..
            } => {
                let child_count = 2;
                let children = (0..child_count)
                    .map(|index| {
                        serde_json::from_value::<TaskDraft>(json!({
                            "title": format!("[retry {}/{}] {}", index + 1, child_count, last_error),
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

fn is_sqlite_busy(err: &SqliteError) -> bool {
    matches!(
        err,
        SqliteError::SqliteFailure(error, _)
            if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn is_busy_message(err: &str) -> bool {
    err.contains("database is busy") || err.contains("database is locked")
}

fn issue_ids_for_label(repo: &Path, label: &str) -> Result<Vec<String>, SqliteError> {
    let conn = rusqlite::Connection::open(repo.join(".beads/beads.db"))?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let mut stmt =
        conn.prepare("SELECT issue_id FROM labels WHERE label = ?1 ORDER BY issue_id")?;
    let ids = stmt
        .query_map(params![label], |row| row.get::<_, String>(0))?
        .collect();
    ids
}

fn insert_dependency_cycle(repo: &Path, ids: &[String]) -> Result<(), SqliteError> {
    let conn = rusqlite::Connection::open(repo.join(".beads/beads.db"))?;
    conn.busy_timeout(Duration::from_millis(100))?;
    for (issue_id, depends_on_id) in [(&ids[0], &ids[1]), (&ids[1], &ids[0])] {
        conn.execute(
            "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by)
             VALUES (?1, ?2, 'blocks', 'signal-dedup-test')",
            params![issue_id, depends_on_id],
        )?;
    }
    Ok(())
}

async fn inject_cycle_when_children_exist(repo: PathBuf, mutation_id: Uuid) -> Result<(), String> {
    let label = labels::mutation_id_label(&mutation_id);
    for _ in 0..2_000 {
        let mut ids = match issue_ids_for_label(&repo, &label) {
            Ok(ids) => ids,
            Err(err) if is_sqlite_busy(&err) => {
                sleep(Duration::from_millis(2)).await;
                continue;
            }
            Err(err) => return Err(err.to_string()),
        };
        if ids.len() >= 2 {
            ids.sort();
            match insert_dependency_cycle(&repo, &ids) {
                Ok(()) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) => {
                    sleep(Duration::from_millis(2)).await;
                    continue;
                }
                Err(err) => return Err(err.to_string()),
            }
        }
        sleep(Duration::from_millis(2)).await;
    }
    Err("timed out waiting for mutation children before injecting cycle".into())
}

async fn tick_once_retrying_busy<P, S>(watcher: &SignalWatcher<P, S>, context: &str)
where
    P: MutationProposer,
    S: MutationScorer,
{
    for _ in 0..2_000 {
        match watcher.tick_once().await {
            Ok(()) => return,
            Err(err) if is_busy_message(&err.to_string()) => {
                sleep(Duration::from_millis(2)).await;
                continue;
            }
            Err(err) => panic!("{context}: {err:#}"),
        }
    }
    panic!("{context}: timed out retrying transient database busy errors");
}

#[tokio::test]
async fn duplicate_signal_comments_with_same_signal_id_commit_once() {
    let _guard = signal_test_guard().await;
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-dedup-1").await;
    let signal_id = Uuid::new_v4();
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

    let signal = scope_drift_signal(signal_id);
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
    let escalation_count = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::EscalationRequested { .. }))
        .count();

    assert_eq!(
        escalation_count, 1,
        "duplicate signal_id must produce exactly one EscalationRequested sentinel; audits={audits:?}"
    );
    assert!(
        !audits.iter().any(|sentinel| {
            matches!(
                sentinel,
                AuditSentinelKind::MutationPlan { .. } | AuditSentinelKind::MutationCommit { .. }
            )
        }),
        "scope_drift must not invoke the proposer/mutation path; audits={audits:?}"
    );
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue.labels.contains(&SIGNAL_ESCALATED_LABEL.to_string()));
    assert!(issue
        .labels
        .contains(&labels::signal_processed_label(&signal_id)));
    assert!(
        !issue
            .labels
            .iter()
            .any(|label| label == labels::READY_FOR_REVIEW || label == "ready-for-review"),
        "escalation must remove ready-for-review labels; labels={:?}",
        issue.labels
    );
}

#[tokio::test]
async fn watcher_skips_signal_task_without_ready_for_review_label() {
    let _guard = signal_test_guard().await;
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

#[tokio::test]
async fn watcher_projects_real_plan_state_for_scoring() {
    let _guard = signal_test_guard().await;
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-projector-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("retry-exhausted"),
        ],
    )
    .await;
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

    let signal = retry_exhausted_signal(Uuid::new_v4(), &task_id);
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ProjectedPlanRetryProposer {
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
        "watcher must use projected persisted plan state to drive retry-exhausted scoring"
    );
}

#[tokio::test]
async fn watcher_processes_only_one_signal_per_task_per_tick() {
    let _guard = signal_test_guard().await;
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
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

    let advanced = pm.advanced().expect("advanced beads surface");
    let first_signal_id = Uuid::new_v4();
    let second_signal_id = Uuid::new_v4();
    advanced
        .add_comment(
            &task_id,
            &signals::encode_comment(&scope_drift_signal(first_signal_id)),
        )
        .await
        .expect("first signal comment");
    advanced
        .add_comment(
            &task_id,
            &signals::encode_comment(&scope_drift_signal(second_signal_id)),
        )
        .await
        .expect("second signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
        common::server_builder::pro_feature_gate(),
    );
    watcher.tick_once().await.expect("tick_once");

    let comments = advanced
        .list_comments(&task_id)
        .await
        .expect("list comments after watcher tick");
    let audits = audit_sentinels(&comments);
    let escalations = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::EscalationRequested { .. }))
        .count();
    assert_eq!(
        escalations, 1,
        "watcher must commit at most one signal decision per task per tick; audits={audits:?}"
    );
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue
        .labels
        .contains(&labels::signal_processed_label(&first_signal_id)));
    assert!(
        !issue
            .labels
            .contains(&labels::signal_processed_label(&second_signal_id)),
        "second signal must wait behind the first per-task signal decision"
    );
}

#[tokio::test]
async fn watcher_skips_review_rejected_tasks_even_if_signal_label_exists() {
    let _guard = signal_test_guard().await;
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
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

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

#[tokio::test]
async fn watcher_reserves_signal_before_mutation_and_does_not_retry_on_failure() {
    let _guard = signal_test_guard().await;
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-retry-1").await;
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("retry-exhausted"),
        ],
    )
    .await;
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

    let signal_id = Uuid::new_v4();
    let signal = retry_exhausted_signal(signal_id, &task_id);
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(&task_id, &signals::encode_comment(&signal))
        .await
        .expect("signal comment");

    let first_mutation_id = Uuid::new_v4();
    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        FixedMutationIdProposer::new(vec![first_mutation_id]),
        TrivialScorer,
        common::server_builder::pro_feature_gate(),
    );

    let first_injector = tokio::spawn(inject_cycle_when_children_exist(
        dir.path().to_path_buf(),
        first_mutation_id,
    ));
    tick_once_retrying_busy(&watcher, "first tick_once").await;
    first_injector
        .await
        .expect("first injector task panicked")
        .expect("first cycle injector failed");

    let first_issue = pm
        .get_issue(&task_id)
        .await
        .expect("load task after first tick");
    assert!(
        first_issue
            .labels
            .iter()
            .any(|label| label == &labels::signal_processed_label(&signal_id)),
        "failed mutation must keep the up-front signal reservation"
    );

    tick_once_retrying_busy(&watcher, "second tick_once").await;

    let issue = pm.get_issue(&task_id).await.expect("load task after retry");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == &labels::signal_processed_label(&signal_id)),
        "processed reservation must continue to consume the failed signal"
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
        mutation_plan_count, 1,
        "watcher must not retry a signal reserved before a failed mutation: {audits:?}"
    );
    assert_eq!(
        violation_count, 1,
        "the single failed mutation should leave one invariant-violation breadcrumb: {audits:?}"
    );
}

#[tokio::test]
async fn distinct_signal_on_task_with_prior_processed_label_is_not_skipped() {
    let _guard = signal_test_guard().await;
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = build_single_task_plan(pm.as_ref(), "signal-exact-dedup-1").await;
    let new_signal_id = Uuid::new_v4();
    add_labels_individually(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;
    seed_completion_with_base_oid(pm.as_ref(), &task_id).await;

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

    let new_signal = scope_drift_signal(new_signal_id);
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
    let escalation_count = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::EscalationRequested { .. }))
        .count();

    assert_eq!(
        escalation_count, 1,
        "a distinct signal must be escalated even when the task already carries a different signal-processed label; audits={audits:?}"
    );
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue
        .labels
        .contains(&labels::signal_processed_label(&old_signal_id)));
    assert!(issue
        .labels
        .contains(&labels::signal_processed_label(&new_signal_id)));
}
