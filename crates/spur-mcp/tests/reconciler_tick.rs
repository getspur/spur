//! Integration test: `Reconciler::observe_ready` returns only unblocked tasks
//! under a plan filter.
//!
//! Requires `br` on PATH; skipped otherwise. `bv` is optional — if absent the
//! reconciler falls back to `br ready` which is what this test exercises.
//!
//! Setup:
//!   - temp beads workspace, `br init`
//!   - create epic + 2 tasks; task B depends on task A
//!   - label all three with `spur:plan-id:P1`
//!   - label the epic with `spur:plan-complete`
//!
//! Assertion:
//!   - `observe_ready` returns a list containing task A's id
//!   - task B is NOT in the list (blocked by A)

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig};
use tempfile::TempDir;
use tokio::sync::Notify;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `br <args> --json` in the given directory; panics on failure.
fn run_br_json(repo: &Path, args: &[&str]) -> String {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--json");
    let out = Command::new("br")
        .args(&full_args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        String::from_utf8_lossy(&out.stdout).to_string()
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

/// Run `br <args>` in the given directory (no --json); panics on failure.
fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

/// Extract the `"id"` field from a JSON object returned by `br create --json`.
fn parse_id_from_create(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("br create output not JSON");
    v.get("id")
        .and_then(|id| id.as_str())
        .unwrap_or_else(|| panic!("no 'id' field in br create output: {json}"))
        .to_string()
}

/// Apply a label to an issue via `br label add <id> <label>`.
fn label_issue(repo: &Path, issue_id: &str, label: &str) {
    run_br(repo, &["label", "add", issue_id, label]);
}

#[tokio::test]
async fn observe_ready_returns_unblocked_task_only() {
    if !br_available() {
        eprintln!("skipping reconciler_tick: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // --- Create epic + 2 tasks ---
    let epic_json = run_br_json(
        dir.path(),
        &["create", "--type", "epic", "--title", "Plan P1 Epic", "--priority", "2"],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &["create", "--type", "task", "--title", "Task A (unblocked)", "--priority", "2"],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &["create", "--type", "task", "--title", "Task B (blocked by A)", "--priority", "2"],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    // --- Task B depends on Task A (B is blocked by A) ---
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    // --- Label all three with plan-id:P1 ---
    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);

    // --- Label epic with spur:plan-complete ---
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    // --- Construct PmService ---
    let pm = spur_pm::PmService::try_new(
        None,       // no github_repo
        true,       // beads_enabled
        false,      // github_enabled
        dir.path(),
        None,       // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    // --- Build and invoke reconciler ---
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        Some("P1".into()),
    );

    let ready_ids = reconciler
        .observe_ready()
        .await
        .expect("observe_ready must not fail");

    // Task A is unblocked — must appear.
    assert!(
        ready_ids.contains(&task_a_id),
        "expected task A ({task_a_id}) in ready list; got: {ready_ids:?}"
    );

    // Task B is blocked by A — must NOT appear.
    assert!(
        !ready_ids.contains(&task_b_id),
        "task B ({task_b_id}) is blocked and must not be in ready list; got: {ready_ids:?}"
    );
}
