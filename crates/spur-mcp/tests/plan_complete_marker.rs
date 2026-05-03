//! Integration test: `build_epic_subgraph` emits `spur:plan-complete` on the
//! epic after all children + deps are successfully created.
//!
//! This guards the v0a.2 reconciler invariant: partial plan graphs (epic +
//! some children, created mid-loop before a failure) will NOT carry the
//! `spur:plan-complete` label and therefore the reconciler will not observe
//! them as ready work.
//!
//! Requires `br` on PATH and a writable temp directory. The test is skipped
//! (not failed) when `br` is unavailable, following the pattern in
//! `labels_br_round_trip.rs`.

use std::path::Path;
use std::process::Command;

use spur_mcp::plan::{labels, PlanTask};
use tempfile::TempDir;

mod common;

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
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn minimal_tasks() -> Vec<PlanTask> {
    vec![
        PlanTask {
            task_id: "t1".into(),
            agent: "claude-code-acp".into(),
            task: "Do T1.".into(),
            depends_on: vec![],
            issue_id: None,
            context_files: vec![],
        },
        PlanTask {
            task_id: "t2".into(),
            agent: "claude-code-acp".into(),
            task: "Do T2.".into(),
            depends_on: vec!["t1".into()],
            issue_id: None,
            context_files: vec![],
        },
    ]
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn build_epic_subgraph_emits_plan_complete_on_epic() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    // Construct a PmService backed by this temp beads workspace.
    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService) — beads dir must exist after br init");

    let tasks = minimal_tasks();
    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "P-test",
        "Test Epic",
        None,
        &tasks,
    )
    .await
    .expect("build_epic_subgraph must succeed");

    let epic_id = &subgraph.epic_id;

    // Ask beads for the epic issue and check activation flipped pending -> complete.
    let list_out = run_br(dir.path(), &["show", epic_id]).expect("br show <epic_id> failed");

    // `br show` returns a JSON array; grab the first (and only) element.
    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("br show output must be valid JSON");
    let item = &items[0];

    let label_values: Vec<String> = item
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    assert!(
        label_values.contains(&labels::PLAN_COMPLETE.to_string()),
        "epic {epic_id} must carry '{}' after successful build_epic_subgraph; got labels: {label_values:?}",
        labels::PLAN_COMPLETE
    );
    assert!(
        !label_values.contains(&labels::PLAN_PENDING.to_string()),
        "epic {epic_id} must not retain '{}' after successful build_epic_subgraph; got labels: {label_values:?}",
        labels::PLAN_PENDING
    );
}
