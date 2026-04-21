//! T-I1: write-ahead record appears before a failing mutation returns.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use uuid::Uuid;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
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

fn persisted_signal_processed_label(mutation_id: &Uuid) -> String {
    format!("spur:signal-processed:{}", mutation_id.simple())
}

#[tokio::test]
async fn write_ahead_comment_persists_when_rewire_validation_fails() {
    if !br_available() {
        eprintln!(
            "skipping write_ahead_comment_persists_when_rewire_validation_fails: `br` not on PATH"
        );
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

    let batch = mutation_batch(
        Uuid::new_v4(),
        Some(Uuid::new_v4()),
        parent.clone(),
        vec![PlanMutationOp::SplitTask {
            parent: parent.clone(),
            children: vec![task_draft("Only child", "One child so idx 1 is invalid")],
            dep_rewire: DepRewirePolicy::Explicit {
                edges: vec![(1, downstream.clone())],
            },
        }],
    );

    let err = apply_mutation(pm.clone(), &batch)
        .await
        .expect_err("invalid rewire must fail");
    assert!(
        format!("{err:#}").contains("out of range"),
        "expected child index validation error, got: {err:#}"
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
            AuditSentinelKind::MutationPlan { mutation_id, .. }
                if mutation_id == &batch.mutation_id.to_string()
        )),
        "MutationPlan sentinel missing after failing mutation: {sentinels:?}"
    );
    assert!(
        !sentinels.iter().any(|sentinel| matches!(
            sentinel,
            AuditSentinelKind::MutationCommit { mutation_id, .. }
                if mutation_id == &batch.mutation_id.to_string()
        )),
        "MutationCommit must not be emitted on failing mutation: {sentinels:?}"
    );

    let parent_issue = pm.get_issue(&parent).await.expect("load parent");
    let processed_label = persisted_signal_processed_label(&batch.mutation_id);
    assert!(
        !parent_issue
            .labels
            .iter()
            .any(|label| label == &processed_label),
        "signal processed label must not be set when mutation fails early; labels={:?}",
        parent_issue.labels
    );
}
