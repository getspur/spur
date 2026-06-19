//! Focused integration test for global plan-task enumeration with a large backlog.

use std::path::Path;
use std::sync::Arc;

use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig};
use spur_mcp::plan::PlanTask;
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

/// Run `br <args>` in the given directory; panics on failure.
fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

fn plan_task(task_id: &str) -> PlanTask {
    PlanTask {
        task_id: task_id.to_string(),
        agent: "codex".to_string(),
        task: format!("Do {task_id}."),
        depends_on: Vec::new(),
        issue_id: None,
        issue_title: None,
        context_files: Vec::new(),
    }
}

#[tokio::test]
async fn plan_enumeration_finds_tasks_buried_under_backlog() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let mut backlog_jsonl = String::new();
    for idx in 0..1001 {
        let issue = serde_json::json!({
            "id": format!("bd-b{idx:04}"),
            "title": format!("High-priority backlog {idx:04}"),
            "status": "open",
            "priority": 1,
            "issue_type": "task",
            "created_at": "2026-04-30T00:00:00Z",
            "created_by": "test",
            "updated_at": "2026-04-30T00:00:00Z",
            "source_repo": ".",
            "compaction_level": 0,
            "original_size": 0,
        });
        backlog_jsonl.push_str(&serde_json::to_string(&issue).expect("serialize backlog issue"));
        backlog_jsonl.push('\n');
    }
    std::fs::write(dir.path().join(".beads/issues.jsonl"), backlog_jsonl)
        .expect("write backlog jsonl");
    run_br(dir.path(), &["sync", "--import-only"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let tasks = vec![plan_task("t1"), plan_task("t2")];
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "buried-plan",
        "Buried Plan",
        None,
        &tasks,
    )
    .await
    .expect("plan subgraph");
    let task_1_id = subgraph
        .task_map
        .get("t1")
        .expect("task map contains t1")
        .clone();
    let task_2_id = subgraph
        .task_map
        .get("t2")
        .expect("task map contains t2")
        .clone();

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        None,
        common::server_builder::pro_feature_gate(),
    );

    let summaries = reconciler
        .observe_ready_summaries()
        .await
        .expect("observe_ready_summaries");
    let ready_ids = summaries
        .iter()
        .map(|ready| ready.summary.id.as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        ready_ids.len(),
        2,
        "global reconciler should return only ready plan tasks; got: {summaries:?}"
    );
    assert!(
        ready_ids.contains(task_1_id.as_str()),
        "expected t1 ({task_1_id}) in ready summaries; got: {summaries:?}"
    );
    assert!(
        ready_ids.contains(task_2_id.as_str()),
        "expected t2 ({task_2_id}) in ready summaries; got: {summaries:?}"
    );
}
