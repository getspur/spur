//! T-I2: post-mutation cycle triggers compensating rollback.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use tokio::time::sleep;
use uuid::Uuid;

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

fn persisted_mutation_label(mutation_id: &Uuid) -> String {
    format!("spur:mutation-id:{}", mutation_id.simple())
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

async fn inject_cycle_when_children_exist(repo: PathBuf, mutation_id: Uuid) -> Result<(), String> {
    let label = persisted_mutation_label(&mutation_id);
    for _ in 0..2_000 {
        let mut ids = issue_ids_for_label(&repo, &label)?;
        if ids.len() >= 2 {
            ids.sort();
            let sql = format!(
                "INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by) \
                 VALUES ('{}', '{}', 'blocks', 'mutation-test'); \
                 INSERT OR IGNORE INTO dependencies(issue_id, depends_on_id, type, created_by) \
                 VALUES ('{}', '{}', 'blocks', 'mutation-test');",
                ids[0], ids[1], ids[1], ids[0]
            );
            run_sql(&repo, &sql)?;
            return Ok(());
        }
        sleep(Duration::from_millis(2)).await;
    }
    Err("timed out waiting for mutation children before injecting cycle".into())
}

#[tokio::test]
async fn cycle_detection_emits_violation_and_rolls_back() {
    if !br_available() {
        eprintln!("skipping cycle_detection_emits_violation_and_rolls_back: `br` not on PATH");
        return;
    }
    if !sqlite_available() {
        eprintln!("skipping cycle_detection_emits_violation_and_rolls_back: `sqlite3` not on PATH");
        return;
    }

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
        mutation_id,
    ));

    let apply_result = apply_mutation(pm.clone(), &batch).await;
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
    assert!(
        sentinels.iter().any(|sentinel| matches!(
            sentinel,
            AuditSentinelKind::MutationInvariantViolation {
                mutation_id,
                rollback_status,
                ..
            } if mutation_id == &batch.mutation_id.to_string() && rollback_status == "completed"
        )),
        "MutationInvariantViolation sentinel missing after rollback: err={err:#} sentinels={sentinels:?}"
    );
    assert!(
        !sentinels.iter().any(|sentinel| matches!(
            sentinel,
            AuditSentinelKind::MutationCommit { mutation_id, .. }
                if mutation_id == &batch.mutation_id.to_string()
        )),
        "MutationCommit must not be emitted on rolled-back mutation: err={err:#} sentinels={sentinels:?}"
    );

    let child_ids = issue_ids_for_label(dir.path(), &persisted_mutation_label(&batch.mutation_id))
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
