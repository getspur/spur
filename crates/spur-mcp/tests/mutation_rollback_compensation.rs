//! T-v0d-6: rollback audit enumerates succeeded and failed compensations.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Connection, Error as SqliteError, ErrorCode};
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

fn open_test_db(repo: &Path) -> Result<Connection, String> {
    let conn = Connection::open(repo.join(".beads/beads.db")).map_err(|err| err.to_string())?;
    conn.busy_timeout(Duration::from_millis(100))
        .map_err(|err| err.to_string())?;
    Ok(conn)
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

fn inject_rollback_failure_raw(
    repo: &Path,
    ids: &[String],
    downstream: &str,
) -> Result<(), SqliteError> {
    let mut conn = Connection::open(repo.join(".beads/beads.db"))?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let tx = conn.transaction()?;
    for (issue_id, depends_on_id) in [(&ids[0], &ids[1]), (&ids[1], &ids[0])] {
        tx.execute(
            "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by)
             VALUES (?1, ?2, 'blocks', 'mutation-test')",
            params![issue_id, depends_on_id],
        )?;
    }
    tx.execute("DELETE FROM issues WHERE id = ?1", params![downstream])?;
    tx.commit()?;
    Ok(())
}

fn downstream_child_edge_count(
    repo: &Path,
    downstream: &str,
    child_ids: &[String],
) -> Result<usize, String> {
    let conn = open_test_db(repo)?;
    let mut count = 0usize;
    for child_id in child_ids {
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dependencies WHERE issue_id = ?1 AND depends_on_id = ?2",
                params![downstream, child_id],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;
        if rows > 0 {
            count += 1;
        }
    }
    Ok(count)
}

async fn inject_partial_rollback_failure(
    repo: PathBuf,
    pm: Arc<spur_pm::PmService>,
    mutation_id: Uuid,
    downstream: String,
    expected_child_count: usize,
) -> Result<(), String> {
    let label = mutation_id_label(&mutation_id);
    let mut child_ids = None;
    for _ in 0..5_000 {
        let current_child_ids = match issue_ids_for_label_pm(pm.as_ref(), &label).await {
            Ok(ids) => ids,
            Err(err) if is_busy_message(&err) => {
                sleep(Duration::from_millis(2)).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        if current_child_ids.len() >= expected_child_count {
            child_ids = Some(current_child_ids);
            break;
        }
        sleep(Duration::from_millis(2)).await;
    }
    let child_ids =
        child_ids.ok_or("timed out waiting for mutation children before injecting cycle")?;

    for _ in 0..5_000 {
        let edge_count = match downstream_child_edge_count(&repo, &downstream, &child_ids) {
            Ok(count) => count,
            Err(err) if is_busy_message(&err) => {
                sleep(Duration::from_millis(2)).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        if edge_count >= expected_child_count {
            match inject_rollback_failure_raw(&repo, &child_ids, &downstream) {
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
    Err("timed out waiting for downstream child rewrites before rollback failure injection".into())
}

#[tokio::test]
async fn t_v0d_6_rollback_audit_payload_enumerates_succeeded_and_failed_compensations() {
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
    let expected_child_count = 12usize;
    let batch = mutation_batch(
        mutation_id,
        Some(Uuid::new_v4()),
        parent.clone(),
        vec![PlanMutationOp::SplitTask {
            parent: parent.clone(),
            children: (0..expected_child_count)
                .map(|idx| task_draft(&format!("Child {idx}"), &format!("Split child {idx}")))
                .collect(),
            dep_rewire: DepRewirePolicy::Barrier,
        }],
    );

    let injector = tokio::spawn(inject_partial_rollback_failure(
        dir.path().to_path_buf(),
        pm.clone(),
        mutation_id,
        downstream.clone(),
        expected_child_count,
    ));

    let apply_result = apply_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        &batch,
    )
    .await;
    injector
        .await
        .expect("partial rollback injector task panicked")
        .expect("partial rollback injector failed");
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
        } if mutation_id == &batch.mutation_id.to_string()
            && rollback_status.starts_with("failed:") =>
        {
            Some((rollback_status, rollback_ops_succeeded, rollback_ops_failed))
        }
        _ => None,
    });
    assert!(
        violation.is_some(),
        "MutationInvariantViolation sentinel missing after partial rollback: err={err:#} sentinels={sentinels:?}"
    );

    let (rollback_status, rollback_ops_succeeded, rollback_ops_failed) =
        violation.expect("MutationInvariantViolation must exist");
    assert!(
        !rollback_ops_succeeded.is_empty(),
        "partial rollback must still report succeeded compensation ops"
    );
    assert!(
        !rollback_ops_failed.is_empty(),
        "partial rollback must report failed compensation ops"
    );
    assert!(
        rollback_status.contains("rollback compensation op(s) failed"),
        "rollback status should remain human-readable: {rollback_status}"
    );

    assert!(
        rollback_ops_succeeded.iter().all(|op| matches!(
            op.kind.as_str(),
            "remove_dependency"
                | "restore_dependency"
                | "close_child_issue"
                | "restore_parent_status"
                | "clear_superseded_by_label"
        )),
        "successful rollback ops must stay human-readable: {rollback_ops_succeeded:?}"
    );
    assert!(
        rollback_ops_failed.iter().all(|(op, error)| {
            matches!(
                op.kind.as_str(),
                "remove_dependency"
                    | "restore_dependency"
                    | "close_child_issue"
                    | "restore_parent_status"
                    | "clear_superseded_by_label"
            ) && !error.trim().is_empty()
        }),
        "failed rollback ops must keep human-readable op kinds and non-empty errors: {rollback_ops_failed:?}"
    );
    assert!(
        rollback_ops_failed.iter().any(|(op, error)| {
            op.kind == "restore_dependency"
                && op.issue_id == downstream
                && op.depends_on_id.as_deref() == Some(parent.as_str())
                && error.contains(&downstream)
        }),
        "partial rollback should record the deleted downstream restore failure: {rollback_ops_failed:?}"
    );
}
