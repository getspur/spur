//! T-I2: post-mutation cycle triggers compensating rollback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Error as SqliteError, ErrorCode};
use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels::mutation_id_label;
use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use tokio::time::sleep;
use uuid::Uuid;

mod common;

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn br_id(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

fn sentinels_from_comments(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

fn task_draft(title: &str, description: &str) -> TaskDraft {
    serde_json::from_value(json!({
        "title": title,
        "description": description,
        "assignee": null,
        "priority": null
    }))
    .expect("TaskDraft JSON must deserialize")
}

fn mutation_batch(
    mutation_id: Uuid,
    trigger_signal_id: Option<Uuid>,
    trigger_task_id: String,
    ops: Vec<PlanMutationOp>,
) -> MutationBatch {
    serde_json::from_value(json!({
        "mutation_id": mutation_id,
        "trigger_signal_id": trigger_signal_id,
        "trigger_task_id": trigger_task_id,
        "ops": ops
    }))
    .expect("MutationBatch JSON must deserialize")
}

async fn issue_ids_for_label_pm(
    pm: &spur_pm::PmService,
    label: &str,
) -> Result<Vec<String>, String> {
    let mut ids = pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![label.to_string()],
            include_closed: true,
            limit: Some(1_000),
            ..Default::default()
        })
        .await
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|issue| issue.id)
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
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

fn insert_dependency_cycle(repo: &Path, ids: &[String]) -> Result<(), SqliteError> {
    let conn = rusqlite::Connection::open(repo.join(".beads/beads.db"))?;
    conn.busy_timeout(Duration::from_millis(100))?;
    for (issue_id, depends_on_id) in [(&ids[0], &ids[1]), (&ids[1], &ids[0])] {
        conn.execute(
            "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by)
             VALUES (?1, ?2, 'blocks', 'mutation-test')",
            params![issue_id, depends_on_id],
        )?;
    }
    Ok(())
}

async fn inject_cycle_when_children_exist(
    repo: PathBuf,
    pm: Arc<spur_pm::PmService>,
    mutation_id: Uuid,
) -> Result<(), String> {
    let label = mutation_id_label(&mutation_id);
    for _ in 0..2_000 {
        let ids = match issue_ids_for_label_pm(pm.as_ref(), &label).await {
            Ok(ids) => ids,
            Err(err) if is_busy_message(&err) => {
                sleep(Duration::from_millis(2)).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        if ids.len() >= 2 {
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

#[tokio::test]
async fn cycle_detection_emits_violation_and_rolls_back() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let parent =
        br_id(&run_br(dir.path(), &["create", "Parent", "--silent", "-t", "task"]).unwrap());
    let downstream = br_id(
        &run_br(
            dir.path(),
            &["create", "Downstream", "--silent", "-t", "task"],
        )
        .unwrap(),
    );
    run_br(dir.path(), &["dep", "add", &downstream, &parent]).expect("seed downstream dep");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads-backed PmService"),
    );

    let mutation_id = Uuid::new_v4();
    let mut children = Vec::new();
    for idx in 0..12 {
        children.push(task_draft(
            &format!("Child {idx}"),
            &format!("Split child {idx}"),
        ));
    }
    let batch = mutation_batch(
        mutation_id,
        Some(Uuid::new_v4()),
        parent.clone(),
        vec![PlanMutationOp::SplitTask {
            parent: parent.clone(),
            children,
            dep_rewire: DepRewirePolicy::Barrier,
        }],
    );

    let injector = tokio::spawn(inject_cycle_when_children_exist(
        dir.path().to_path_buf(),
        pm.clone(),
        mutation_id,
    ));

    let apply_result = apply_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        &batch,
    )
    .await;
    let inject_result = injector
        .await
        .expect("cycle injector task panicked")
        .map_err(|err| format!("{err}; apply_result={apply_result:?}"));
    inject_result.expect("cycle injector failed");
    let err = apply_result.expect_err("injected cycle must force rollback");

    assert!(
        err.to_string().contains("cycle"),
        "expected cycle error, got: {err:#}"
    );

    let comments = pm
        .advanced()
        .expect("advanced surface")
        .list_comments(&parent)
        .await
        .expect("list comments");
    let sentinels = sentinels_from_comments(&comments);
    let violation = sentinels.iter().find_map(|sentinel| match sentinel {
        AuditSentinelKind::MutationInvariantViolation {
            mutation_id,
            rollback_status,
            rollback_ops_succeeded,
            rollback_ops_failed,
            ..
        } if mutation_id == &batch.mutation_id.to_string() && rollback_status == "completed" => {
            Some((rollback_ops_succeeded, rollback_ops_failed))
        }
        _ => None,
    });
    assert!(
        violation.is_some(),
        "MutationInvariantViolation sentinel missing after rollback: err={err:#} sentinels={sentinels:?}"
    );
    let (rollback_ops_succeeded, rollback_ops_failed) =
        violation.expect("MutationInvariantViolation must exist");
    assert!(
        !rollback_ops_succeeded.is_empty(),
        "rollback audit must describe successful compensation ops"
    );
    assert!(
        rollback_ops_failed.is_empty(),
        "full rollback path must not report failed compensation ops: {rollback_ops_failed:?}"
    );
    let rollback_kinds = rollback_ops_succeeded
        .iter()
        .map(|op| op.kind.as_str())
        .collect::<Vec<_>>();
    let expected_child_count = 12usize;
    let expected_remove_dependency_count = (expected_child_count * 2) + 2;
    let expected_total_count =
        expected_remove_dependency_count + 1 + expected_child_count + 1 + expected_child_count;
    assert!(
        rollback_kinds.iter().all(|kind| matches!(
            *kind,
            "remove_dependency"
                | "restore_dependency"
                | "close_child_issue"
                | "restore_parent_status"
                | "clear_superseded_by_label"
        )),
        "rollback op kinds must stay human-readable: {rollback_kinds:?}"
    );
    assert_eq!(
        rollback_kinds.len(),
        expected_total_count,
        "rollback should record the full compensation surface: {rollback_kinds:?}"
    );
    assert!(
        rollback_kinds[..expected_remove_dependency_count]
            .iter()
            .all(|kind| *kind == "remove_dependency"),
        "rollback must clear child-touched dependencies first: {rollback_kinds:?}"
    );
    assert_eq!(
        rollback_kinds[expected_remove_dependency_count],
        "restore_dependency",
        "rollback must restore downstream -> parent after removing child-touched edges: {rollback_kinds:?}"
    );
    assert!(
        rollback_kinds[expected_remove_dependency_count + 1
            ..expected_remove_dependency_count + 1 + expected_child_count]
            .iter()
            .all(|kind| *kind == "close_child_issue"),
        "rollback must then close all child issues: {rollback_kinds:?}"
    );
    assert_eq!(
        rollback_kinds[expected_remove_dependency_count + 1 + expected_child_count],
        "restore_parent_status",
        "rollback must restore the parent before clearing superseded markers: {rollback_kinds:?}"
    );
    assert!(
        rollback_kinds[expected_remove_dependency_count + 2 + expected_child_count..]
            .iter()
            .all(|kind| *kind == "clear_superseded_by_label"),
        "rollback must finish by clearing superseded labels: {rollback_kinds:?}"
    );
    assert!(
        !sentinels.iter().any(|sentinel| matches!(
            sentinel,
            AuditSentinelKind::MutationCommit { mutation_id, .. }
                if mutation_id == &batch.mutation_id.to_string()
        )),
        "MutationCommit must not be emitted on rolled-back mutation: err={err:#} sentinels={sentinels:?}"
    );

    let child_ids = issue_ids_for_label_pm(pm.as_ref(), &mutation_id_label(&batch.mutation_id))
        .await
        .expect("query rollback children by label");
    assert_eq!(
        child_ids.len(),
        12,
        "expected all rollback children to exist"
    );
    for child_id in &child_ids {
        let child = pm.get_issue(child_id).await.expect("load rollback child");
        assert_eq!(
            child.status,
            pm.closed_status(),
            "rolled-back child {child_id} must be mapped to backend closed status"
        );
    }

    let downstream_issue = pm.get_issue(&downstream).await.expect("load downstream");
    assert!(
        downstream_issue.blocked_by.iter().any(|dep| dep == &parent),
        "rollback must restore downstream -> parent dependency; blocked_by={:?}",
        downstream_issue.blocked_by
    );
    for child in &child_ids {
        assert!(
            !downstream_issue.blocked_by.iter().any(|dep| dep == child),
            "rollback must remove downstream -> child dependency for {child}; blocked_by={:?}",
            downstream_issue.blocked_by
        );
    }

    let cycles = pm
        .advanced()
        .expect("advanced surface")
        .dep_cycles()
        .await
        .expect("dep_cycles");
    assert!(
        cycles.is_empty(),
        "rollback must leave the graph acyclic: {cycles:?}"
    );
}
