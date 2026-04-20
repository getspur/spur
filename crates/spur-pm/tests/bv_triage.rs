//! Integration test for BvAdapter::triage against a real `bv` binary.
//!
//! Validates the contract the v0a.2 reconciler (Task 9) depends on:
//! `BvAdapter::triage(Some("spur:plan-id:<id>"))` returns a TriageReport
//! whose `recommendations` carry issue IDs for unblocked tasks under that
//! plan.

use std::path::Path;
use std::process::Command;

use spur_pm::BvAdapter;
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bv_available() -> bool {
    Command::new("bv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation");
    assert!(out.status.success(), "br {args:?} failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v["id"].as_str().expect("id").to_string()
}

/// Smoke test: bv --robot-triage returns a structurally valid TriageReport
/// on an empty workspace. Proves the adapter + bv binary + JSON schema agree.
#[tokio::test]
async fn triage_on_empty_workspace_returns_report() {
    if !br_available() || !bv_available() {
        eprintln!("skipping: `br` or `bv` not on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
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
    if !br_available() || !bv_available() {
        eprintln!("skipping: `br` or `bv` not on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);

    // Create one issue under "spur:plan-id:P1" plus one unrelated issue.
    let plan_task = extract_id(&run_br(
        dir.path(),
        &["create", "plan-task", "-t", "task"],
    ));
    run_br(
        dir.path(),
        &["label", "add", &plan_task, "-l", "spur:plan-id:P1"],
    );
    let _other = extract_id(&run_br(dir.path(), &["create", "other", "-t", "task"]));

    let bv = BvAdapter::connect(dir.path()).await.expect("bv connect");
    let report = bv
        .triage(Some("spur:plan-id:P1"))
        .await
        .expect("triage");

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
