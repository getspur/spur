//! T-v0d-6: rollback audit enumerates succeeded and failed compensations.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels::mutation_id_label;
use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use tokio::time::sleep;
use uuid::Uuid;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sqlite_available() -> bool {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn run_sql(repo: &Path, sql: &str) -> Result<(), String> {
    let db = repo.join(".beads/beads.db");
    let out = Command::new("sqlite3")
        .arg("-cmd")
        .arg(".timeout 2000")
        .arg(db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "sqlite3 failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn run_sql_json(repo: &Path, sql: &str) -> Result<String, String> {
    let db = repo.join(".beads/beads.db");
    let out = Command::new("sqlite3")
        .arg("-cmd")
        .arg(".timeout 2000")
        .arg("-json")
        .arg(db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "sqlite3 -json failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
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
    let ids = serde_json::from_str::<Vec<serde_json::Value>>(&rows)
        .map_err(|err| format!("parse sqlite label rows: {err}; raw={rows}"))?
        .into_iter()
        .filter_map(|row| {
            row.get("issue_id")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    Ok(ids)
}

async fn inject_partial_rollback_failure(repo: PathBuf, mutation_id: Uuid) -> Result<(), String> {
    let label = mutation_id_label(&mutation_id);
    for _ in 0..2_000 {
        let mut child_ids = issue_ids_for_label(&repo, &label)?;
        child_ids.sort();
        if child_ids.len() >= 2 {
            run_sql(
                &repo,
                &format!(
                    "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by)
                     VALUES ('{}', '{}', 'blocks', 'mutation-test');
                     INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by)
                     VALUES ('{}', '{}', 'blocks', 'mutation-test');",
                    child_ids[0], child_ids[1], child_ids[1], child_ids[0]
                ),
            )?;
            return Ok(());
        }
        sleep(Duration::from_millis(2)).await;
    }
    Err("timed out waiting to inject partial rollback failure".into())
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn t_v0d_6_rollback_audit_payload_enumerates_succeeded_and_failed_compensations() {
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
    run_sql(
        dir.path(),
        &format!(
            "CREATE TRIGGER delete_downstream_after_parent_close
             AFTER UPDATE OF status ON issues
             WHEN NEW.id = '{parent}' AND NEW.status = 'closed'
             BEGIN
               DELETE FROM issues WHERE id = '{downstream}';
             END;"
        ),
    )
    .expect("install downstream deletion trigger");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads-backed PmService"),
    );

    let mutation_id = Uuid::new_v4();
    let batch = mutation_batch(
        mutation_id,
        Some(Uuid::new_v4()),
        parent.clone(),
        vec![PlanMutationOp::SplitTask {
            parent: parent.clone(),
            children: vec![
                task_draft("Child A", "First split child"),
                task_draft("Child B", "Second split child"),
            ],
            dep_rewire: DepRewirePolicy::Barrier,
        }],
    );

    let injector = tokio::spawn(inject_partial_rollback_failure(
        dir.path().to_path_buf(),
        mutation_id,
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
