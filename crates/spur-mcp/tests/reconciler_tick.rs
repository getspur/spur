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

use spur_acp::{BrainSessionId, SessionId, SpurEvent, SpurEventBody};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::McpEventSink;
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

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

/// Run `git <args>` in the given directory; panics on failure.
fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "git {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
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

fn collect_sentinels(list_json: &str) -> Vec<AuditSentinelKind> {
    let items: serde_json::Value =
        serde_json::from_str(list_json).expect("br comments list must be valid JSON");
    items
        .as_array()
        .expect("comments must be JSON array")
        .iter()
        .filter_map(|comment| comment.get("text").and_then(|text| text.as_str()))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(|result| result.ok())
        .collect()
}

fn extract_submit_plan_task_issue_id(response: &serde_json::Value, task_id: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_json = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .expect("submit_plan response must include task_map line");
    let task_map: std::collections::HashMap<String, String> =
        serde_json::from_str(task_map_json).expect("task_map line must be valid JSON");
    task_map
        .get(task_id)
        .cloned()
        .unwrap_or_else(|| panic!("submit_plan task_map must include '{task_id}'"))
}

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEvent>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events.lock().unwrap().push(SpurEvent::now(body));
    }
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
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
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

#[tokio::test]
async fn observe_ready_summaries_preserve_plan_labels() {
    if !br_available() {
        eprintln!("skipping observe_ready_summaries_preserve_plan_labels: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let task_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    );
    let task_id = parse_id_from_create(&task_json);
    label_issue(dir.path(), &task_id, &labels::plan_id("P1"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::new(pm),
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let summaries = reconciler
        .observe_ready_summaries()
        .await
        .expect("ready summaries");
    assert!(summaries.iter().any(|summary| {
        summary.id == task_id && summary.labels.contains(&labels::plan_id("P1"))
    }));
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
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
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

#[tokio::test]
async fn epic_closes_when_scoped_children_terminal() {
    if !br_available() {
        eprintln!("skipping epic_closes_when_scoped_children_terminal: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_json = run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan terminal epic",
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
            "Task A terminal",
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
            "Task B terminal",
            "--priority",
            "2",
        ],
    );
    let task_b_id = parse_id_from_create(&task_b_json);

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    pm.update_issue(
        &task_a_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task A");
    pm.update_issue(
        &task_b_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task B");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler
        .tick_once()
        .await
        .expect("tick_once must succeed");

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());

    let epic_comments = run_br_json(dir.path(), &["comments", "list", &epic_id]);
    let sentinels = collect_sentinels(&epic_comments);
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion { plan_id, epic_id: found_epic_id, .. }
            if plan_id == "P1" && found_epic_id == &epic_id
    )));
}

#[tokio::test]
async fn all_approved_epic_emits_plan_ready_to_merge() {
    if !br_available() {
        eprintln!("skipping all_approved_epic_emits_plan_ready_to_merge: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan ready epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A approved",
            "--priority",
            "2",
        ],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B approved",
            "--priority",
            "2",
        ],
    ));

    let plan_label = labels::plan_id("P1");
    label_issue(dir.path(), &epic_id, &plan_label);
    label_issue(dir.path(), &task_a_id, &plan_label);
    label_issue(dir.path(), &task_b_id, &plan_label);
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    for task_id in [&task_a_id, &task_b_id] {
        pm.update_issue(
            task_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close task");
    }

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (delegation_tx, _delegation_rx) = tokio::sync::mpsc::channel(1);

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: Some(sink_ref),
            materializer: test_materializer(),
        }),
        Some("P1".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler
        .tick_once()
        .await
        .expect("tick_once must succeed");

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.body,
        SpurEventBody::PlanReadyToMerge { plan_id } if plan_id == "P1"
    )));
}

#[tokio::test]
async fn tick_once_persists_dispatch_before_queue_send() {
    if !br_available() {
        eprintln!("skipping tick_once_persists_dispatch_before_queue_send: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(did_work);
    let request = delegation_rx.recv().await.expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn tick_once_persists_failed_completion_when_respond_to_drops() {
    if !br_available() {
        eprintln!(
            "skipping tick_once_persists_failed_completion_when_respond_to_drops: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let task_tracker = tokio_util::task::TaskTracker::new();
    let fast_forward = Arc::new(Notify::new());
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::clone(&fast_forward),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: task_tracker.clone(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    assert!(reconciler.tick_once().await.expect("tick_once"));
    let request = delegation_rx.recv().await.expect("dispatch request");
    let delegation_id = request.id.as_str().to_string();
    drop(request.respond_to);

    task_tracker.close();
    tokio::time::timeout(std::time::Duration::from_secs(2), task_tracker.wait())
        .await
        .expect("completion task should finish");
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        fast_forward.notified(),
    )
    .await
    .expect("completion should fast-forward the reconciler");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, pm.closed_status());
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "plan-1",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("project plan");
    let task = projected
        .tasks
        .iter()
        .find(|task| task.spec.issue_id.as_deref() == Some(task_id.as_str()))
        .expect("projected task");
    assert!(matches!(
        &task.status,
        spur_mcp::plan::PlanTaskStatus::Failed { error }
            if error == "orchestrator disconnected"
    ));

    let sentinels = collect_sentinels(&run_br_json(dir.path(), &["comments", "list", &task_id]));
    assert!(sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            delegation_id: found_delegation_id,
            completion_state: audit_sentinel::CompletionState::Failed,
            result_summary,
            ..
        } if found_delegation_id == &delegation_id
            && result_summary.as_deref() == Some("orchestrator disconnected")
    )));
}

#[tokio::test]
async fn tick_once_clears_dispatch_label_when_send_fails() {
    if !br_available() {
        eprintln!("skipping tick_once_clears_dispatch_label_when_send_fails: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, delegation_rx) = tokio::sync::mpsc::channel(1);
    drop(delegation_rx);

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    let _ = reconciler.tick_once().await;
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));
}

#[tokio::test]
async fn tick_once_skips_broken_plan_and_dispatches_other_ready_work() {
    if !br_available() {
        eprintln!(
            "skipping tick_once_skips_broken_plan_and_dispatches_other_ready_work: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);

    let broken_task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Broken Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &broken_task_id, &labels::plan_id("bogus-plan"));

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Valid Plan Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let valid_task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Valid Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &valid_task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &valid_task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &valid_task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        None,
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler
        .tick_once()
        .await
        .expect("tick_once should skip the broken plan and continue dispatching");
    assert!(did_work, "a valid ready task should still be dispatched");

    let request = delegation_rx.recv().await.expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(valid_task_id.as_str()));
}

#[tokio::test]
async fn resolve_dispatch_orphan_emits_breadcrumb_and_clears_label() {
    if !br_available() {
        eprintln!(
            "skipping resolve_dispatch_orphan_emits_breadcrumb_and_clears_label: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Dispatch Orphan",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::delegation_id("del-A"));

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let adv = pm.advanced().expect("advanced");

    let cleared = spur_mcp::server::resolve_dispatch_orphan(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("resolve dispatch orphan");
    assert!(cleared, "dispatch orphan should be cleared");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(!issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));

    let audits = adv
        .list_comments(&task_id)
        .await
        .expect("list comments")
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect::<Vec<_>>();
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::DispatchOrphanCleared {
            delegation_id,
            reason,
        } if delegation_id == "del-A" && reason == "restart-orphan-cleared"
    )));
}

#[tokio::test]
async fn resolve_dispatch_orphan_preserves_legacy_ready_for_review_marker() {
    if !br_available() {
        eprintln!(
            "skipping resolve_dispatch_orphan_preserves_legacy_ready_for_review_marker: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Legacy Review Ready",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::delegation_id("del-legacy"));
    label_issue(dir.path(), &task_id, "ready-for-review");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);

    let cleared = spur_mcp::server::resolve_dispatch_orphan(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("resolve dispatch orphan");
    assert!(
        !cleared,
        "legacy ready-for-review marker should block dispatch orphan cleanup"
    );

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == &labels::delegation_id("del-legacy")),
        "delegation label must be preserved while task is awaiting review"
    );
}

#[tokio::test]
async fn execute_epic_persists_execution_scope_labels_on_epic_and_tasks() {
    if !br_available() {
        eprintln!("skipping execute_epic_persists_execution_scope_labels_on_epic_and_tasks: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Execute Epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    eprintln!("execute_epic_response={response}");
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert!(epic
        .labels
        .iter()
        .any(|label| label.starts_with("spur:plan-id:")));

    for task_id in [&task_a_id, &task_b_id] {
        let task = pm.get_issue(task_id).await.expect("get task");
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")));
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-task-id:")));
        assert!(task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:agent:")));
    }
}

#[tokio::test]
async fn execute_epic_reprojects_persisted_non_terminal_state_before_starting_fresh_run() {
    if !br_available() {
        eprintln!(
            "skipping execute_epic_reprojects_persisted_non_terminal_state_before_starting_fresh_run: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Execute Epic Twice",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let first = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        first.get("error").is_none(),
        "first execute_epic should succeed: {first}"
    );
    let first_text = first["result"]["content"][0]["text"]
        .as_str()
        .expect("execute_epic response text");
    let first_json: serde_json::Value =
        serde_json::from_str(first_text).expect("execute_epic response JSON");
    let first_plan_id = first_json["plan_id"].as_str().expect("plan_id").to_string();

    server
        .__test_corrupt_cached_plan(
            &first_plan_id,
            &task_a_id,
            "spur/bogus-worker",
            "spur/bogus-snapshot",
        )
        .await
        .expect("corrupt cached plan");

    let second = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        second.get("error").is_none(),
        "second execute_epic should reproject persisted non-terminal state and reuse the active plan: {second}"
    );

    let second_text = second["result"]["content"][0]["text"]
        .as_str()
        .expect("execute_epic response text");
    let second_json: serde_json::Value =
        serde_json::from_str(second_text).expect("execute_epic response JSON");
    let second_plan_id = second_json["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_string();
    assert_eq!(
        second_plan_id, first_plan_id,
        "stale terminal cache must not cause execute_epic to start a fresh plan"
    );
}

#[tokio::test]
async fn execute_epic_rolls_back_epic_scope_when_task_scope_persist_fails() {
    if !br_available() {
        eprintln!("skipping execute_epic_rolls_back_epic_scope_when_task_scope_persist_fails: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Rollback Epic",
            "--priority",
            "2",
        ],
    ));
    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Rollback Task",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "bad/agent".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&epic_id, Some("bad/agent"))
        .await;
    assert!(
        response.get("error").is_some(),
        "execute_epic should fail when task scope label persistence is invalid: {response}"
    );

    let epic = pm.get_issue(&epic_id).await.expect("get epic");
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")),
        "epic scope labels should roll back on task persist failure"
    );
    let task = pm.get_issue(&task_id).await.expect("get task");
    assert!(
        !task
            .labels
            .iter()
            .any(|label| label.starts_with("spur:plan-id:")),
        "task should not retain partially-written plan scope after execute_epic failure"
    );
}

#[tokio::test]
async fn submit_plan_default_notify_path_dispatches_ready_task() {
    if !br_available() {
        eprintln!(
            "skipping submit_plan_default_notify_path_dispatches_ready_task: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("submit_plan_default_notify_path_dispatches_ready_task");

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);

    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");

    let response = server
        .__test_call_submit_plan(serde_json::json!({
            "persist_as_epic": true,
            "epic_title": "Default Notify Persisted Submit Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Dispatch via started reconciler",
                "depends_on": [],
            }]
        }))
        .await;
    let task_id = extract_submit_plan_task_issue_id(&response, "t1");

    let request =
        tokio::time::timeout(std::time::Duration::from_secs(5), channel.request_rx.recv())
            .await
            .expect("started reconciler should dispatch persisted submit_plan work within timeout")
            .expect("dispatch request");

    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn execute_epic_default_notify_path_dispatches_ready_task() {
    if !br_available() {
        eprintln!(
            "skipping execute_epic_default_notify_path_dispatches_ready_task: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("execute_epic_default_notify_path_dispatches_ready_task");

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Default Notify Execute Epic",
            "--priority",
            "2",
        ],
    ));
    let task_a_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    ));
    let task_b_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task B",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_a_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &epic_id]);
    run_br(dir.path(), &["dep", "add", &task_b_id, &task_a_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should succeed: {response}"
    );

    let request =
        tokio::time::timeout(std::time::Duration::from_secs(5), channel.request_rx.recv())
            .await
            .expect("started reconciler should dispatch execute_epic work within timeout")
            .expect("dispatch request");

    assert_eq!(request.issue_id.as_deref(), Some(task_a_id.as_str()));
}

#[tokio::test]
async fn execute_epic_shutdown_abort_does_not_emit_plan_snapshot() {
    if !br_available() {
        eprintln!(
            "skipping execute_epic_shutdown_abort_does_not_emit_plan_snapshot: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Shutdown Abort Execute Epic",
            "--priority",
            "2",
        ],
    ));
    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Task A",
            "--priority",
            "2",
        ],
    ));
    run_br(dir.path(), &["dep", "add", &task_id, &epic_id]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);
    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(Arc::clone(&pm)),
        Some(sink_ref),
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    server.shutdown().await;

    let response = server
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert_eq!(
        response["error"]["message"].as_str(),
        Some("orchestrator shutting down — execute_epic aborted"),
        "execute_epic should abort cleanly when task tracker is closed: {response}"
    );
    assert_eq!(
        server.__test_active_plan_count().await,
        0,
        "shutdown abort should not leave a cached active plan behind"
    );

    let events = sink.events.lock().unwrap();
    let snapshot_count = events
        .iter()
        .filter(|event| matches!(event.body, SpurEventBody::PlanSnapshotUpdated { .. }))
        .count();
    assert_eq!(
        snapshot_count, 0,
        "shutdown abort must not emit a PlanSnapshotUpdated event for a rolled-back plan"
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
    let reconciler = Reconciler::new(
        cfg,
        pm,
        Arc::new(Notify::new()),
        None,
        None,
        common::server_builder::pro_feature_gate(),
    );

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

/// Hybrid wake equivalence: a fast-forward signal causes the reconciler to tick
/// and produce the same observable dispatch as a direct `tick_once()` call.
/// The journal wake is optional and does not change `tick_once()` semantics.
#[tokio::test]
async fn hybrid_fast_forward_matches_polling_projection() {
    use std::time::Duration;

    if !br_available() {
        eprintln!("skipping hybrid_fast_forward_matches_polling_projection: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let epic_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "epic",
            "--title",
            "Plan 1 Epic",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &epic_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &epic_id, labels::PLAN_COMPLETE);

    let task_id = parse_id_from_create(&run_br_json(
        dir.path(),
        &[
            "create",
            "--type",
            "task",
            "--title",
            "Ready Task",
            "--priority",
            "2",
        ],
    ));
    label_issue(dir.path(), &task_id, &labels::plan_id("plan-1"));
    label_issue(dir.path(), &task_id, &labels::plan_task_id("t1"));
    label_issue(dir.path(), &task_id, &labels::agent("codex"));

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let fast_forward = Arc::new(Notify::new());

    let reconciler = Reconciler::new(
        ReconcilerConfig {
            base_interval: Duration::from_secs(60),
            idle_ceiling: Duration::from_secs(60),
            backoff_factor: 2,
        },
        Arc::clone(&pm),
        Arc::clone(&fast_forward),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId::new()),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-1".into()),
        common::server_builder::pro_feature_gate(),
    );

    // Spawn run() with a long interval; without fast-forward it would not tick for 60s.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move { reconciler.run(cancel_rx).await });

    // Yield briefly to let the reconciler enter the select! loop.
    tokio::task::yield_now().await;

    // Fast-forward should trigger an immediate tick and dispatch the ready task.
    fast_forward.notify_one();

    let request = tokio::time::timeout(Duration::from_secs(2), delegation_rx.recv()).await;
    assert!(
        request.is_ok(),
        "fast-forward must trigger tick and dispatch within timeout"
    );
    let request = request.unwrap();
    assert!(
        request.is_some(),
        "fast-forward must produce a delegation request"
    );
    assert_eq!(request.unwrap().issue_id.as_deref(), Some(task_id.as_str()));

    // Cancel and clean up.
    let _ = cancel_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
