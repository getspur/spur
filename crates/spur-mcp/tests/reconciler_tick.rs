//! Integration tests for `Reconciler`.
//!
//! # `observe_ready_returns_unblocked_task_only`
//!
//! Exercises the `observe_ready` bv-primary path (falls through to br if bv
//! absent). Requires `br` on PATH; skipped otherwise.
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
//!
//! # `observe_ready_via_br_returns_ready_tasks`
//!
//! Exercises the br fallback path (`observe_ready_via_br`) directly, bypassing
//! the bv primary path entirely. Verifies the corrected filter (plan_id only,
//! no PLAN_COMPLETE) correctly identifies unblocked tasks.
//!
//! Same fixture as above; calls `observe_ready_via_br()` instead of
//! `observe_ready()`.

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
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan P1 Epic",
            "--priority",
            "2",
        ],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A (unblocked)",
            "--priority",
            "2",
        ],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B (blocked by A)",
            "--priority",
            "2",
        ],
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
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
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

/// Exercises the br fallback path directly via `observe_ready_via_br`.
///
/// This test bypasses the bv primary path and calls the br fallback helper
/// directly. It verifies that the corrected filter (spur:plan-id:<id> only,
/// no spur:plan-complete) correctly identifies unblocked tasks — tasks never
/// carry PLAN_COMPLETE, which is an epic-only marker.
#[tokio::test]
async fn observe_ready_via_br_returns_ready_tasks() {
    if !br_available() {
        eprintln!("skipping observe_ready_via_br: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // --- Create epic + 2 tasks ---
    let epic_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan P1 Epic (br fallback)",
            "--priority",
            "2",
        ],
    );
    let epic_id = parse_id_from_create(&epic_json);

    let task_a_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A (unblocked, br path)",
            "--priority",
            "2",
        ],
    );
    let task_a_id = parse_id_from_create(&task_a_json);

    let task_b_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B (blocked by A, br path)",
            "--priority",
            "2",
        ],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    // --- Task B depends on Task A (B is blocked by A) ---
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    // --- Label all three with plan-id:P1 ---
    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);

    // --- Label epic with spur:plan-complete (tasks do NOT get this label) ---
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    // --- Construct PmService ---
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
    let pm = Arc::new(pm);

    // --- Build reconciler scoped to plan P1 ---
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        Some("P1".into()),
    );

    // --- Call the br fallback path directly (bypasses bv entirely) ---
    let ready_ids = reconciler
        .observe_ready_via_br()
        .await
        .expect("observe_ready_via_br must not fail");

    // Task A is unblocked — must appear in the br fallback result.
    assert!(
        ready_ids.contains(&task_a_id),
        "expected task A ({task_a_id}) in br fallback ready list; got: {ready_ids:?}"
    );

    // Task B is blocked by A — must NOT appear.
    assert!(
        !ready_ids.contains(&task_b_id),
        "task B ({task_b_id}) is blocked and must not be in br fallback ready list; got: {ready_ids:?}"
    );
}

/// D1 regression: `Reconciler::run` must honor cancel even while a tick is
/// mid-flight. Prior to the biased cancel-race select, cancel could only win
/// between ticks; a stuck `bv.triage`/`br ready` would hang shutdown.
///
/// Strategy: spin the reconciler at a very fast cadence so ticks are nearly
/// always in flight, let it run for a brief warm-up, then fire cancel. With
/// the fix in place the spawned task must complete within the 1-second
/// timeout budget.
#[tokio::test]
async fn reconciler_cancels_during_tick() {
    use std::time::Duration;

    if !br_available() {
        eprintln!("skipping reconciler_cancels_during_tick: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    // Seed a few issues so tick_once has non-trivial work to process.
    for idx in 0..5 {
        run_br_json(
            dir.path(),
            &[
                "create",
                "--type",
                "task",
                "--title",
                &format!("Warm-up task {idx}"),
                "--priority",
                "2",
            ],
        );
    }

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    // Very fast cadence → high probability that cancel lands mid-tick.
    let cfg = ReconcilerConfig {
        base_interval: Duration::from_millis(5),
        idle_ceiling: Duration::from_millis(50),
        backoff_factor: 2,
    };
    let reconciler = Reconciler::new(cfg, pm, Arc::new(Notify::new()), None);

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move { reconciler.run(cancel_rx).await });

    // Let the run loop fire several ticks.
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancel_tx.send(()).expect("cancel receiver alive");

    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("reconciler must shut down within 1s of cancel")
        .expect("reconciler task must not panic");
}
