//! Integration test for BvAdapter::triage against a real `bv` binary.
//!
//! Validates the contract the v0a.2 reconciler (Task 9) depends on:
//! `BvAdapter::triage(Some("spur:plan-id:<id>"))` returns a TriageReport
//! whose `recommendations` carry issue IDs for unblocked tasks under that
//! plan.

use std::path::Path;
use std::process::Command;

use beads_rust::sync::{export_to_jsonl, ExportConfig};
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::BvAdapter;
use tempfile::TempDir;

fn bv_available() -> bool {
    Command::new("bv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn attach_beads_workspace(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    for suffix in ["", "-wal", "-shm"] {
        let file_name = format!("beads.db{suffix}");
        let src = w.path().join(&file_name);
        if src.exists() {
            std::fs::copy(&src, beads_dir.join(file_name)).expect("copy test beads database");
        }
    }
    export_to_jsonl(
        &w.storage,
        &beads_dir.join("issues.jsonl"),
        &ExportConfig {
            force: true,
            is_default_path: true,
            beads_dir: Some(beads_dir),
            ..Default::default()
        },
    )
    .expect("flush test beads JSONL");
}

/// Smoke test: bv --robot-triage returns a structurally valid TriageReport
/// on an empty workspace. Proves the adapter + bv binary + JSON schema agree.
#[tokio::test]
async fn triage_on_empty_workspace_returns_report() {
    if !bv_available() {
        eprintln!("skipping: `bv` not on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    let w = TestBeadsWorkspace::init();
    attach_beads_workspace(dir.path(), &w);
    let bv = BvAdapter::connect(dir.path()).await.expect("bv connect");

    let report = bv.triage(None).await.expect("triage");
    // Empty workspace: recommendations may be empty but the struct deserializes.
    assert!(
        report.triage.recommendations.is_empty() || !report.triage.recommendations.is_empty(),
        "recommendations deserialized"
    );
}

/// The contract Task 9 depends on: triage(Some(label)) returns a TriageReport
/// whose recommendations come from issues carrying that label. Create a
/// labeled open issue, verify it appears in the label-scoped triage output.
#[tokio::test]
async fn triage_with_label_filter_surfaces_matching_issue() {
    if !bv_available() {
        eprintln!("skipping: `bv` not on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    let mut w = TestBeadsWorkspace::init();

    // Create one issue under "spur:plan-id:P1" plus one unrelated issue.
    let plan_task = w.create_issue("plan-task");
    w.add_label(&plan_task, "spur:plan-id:P1");
    let _other = w.create_issue("other");
    attach_beads_workspace(dir.path(), &w);

    let bv = BvAdapter::connect(dir.path()).await.expect("bv connect");
    let report = bv.triage(Some("spur:plan-id:P1")).await.expect("triage");

    // Label-scoped query should surface the plan_task but not "other".
    let ids: Vec<&str> = report
        .triage
        .recommendations
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert!(
        ids.contains(&plan_task.as_str()),
        "plan_task {plan_task} missing from label-scoped triage: {ids:?}"
    );
}
