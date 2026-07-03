//! Integration tests for the `ResumePlan` orchestrator bridge (bd-14in).
//!
//! ## What is tested
//!
//! The `InteractiveInput::ResumePlan { plan_id }` arm in `run_interactive`:
//!
//! | Test | Condition | Expected event |
//! |------|-----------|----------------|
//! | `no_brain_session_emits_plan_command_error` | No active brain session | `PlanCommandError { operation: "ResumePlan", error: "No active brain session — start one to resume plans" }` |
//! | `retry_plan_task_no_brain_session_emits_plan_command_error` | No active brain session | `PlanCommandError { operation: "RetryPlanTask", error: "No active brain session — start one to retry plan tasks" }` |
//! | `mcp_server_plan_owned_by_other_emits_plan_command_error` | Brain with MCP server; plan owned by different session | `PlanCommandError` with ownership-conflict message from `call_resume_plan` |
//!
//! ## Harness note
//!
//! Test 1 follows the orchestrator harness pattern from
//! `session_milestone_events.rs` / `brain_error_session_correlation.rs` —
//! spawn `run_interactive` as a background task, subscribe to the broadcast
//! receiver, drive inputs via the mpsc sender.
//!
//! Test 2 drives `McpCallbackServer::call_resume_plan` directly (the public
//! bridge method the orchestrator calls).  The orchestrator loop cannot be
//! seeded with an active `BrainSession` without a live ACP transport, so this
//! test validates the bridge's downstream at its natural boundary.

// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use spur_acp::config::SpurConfig;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::new_overflow_buf;
use spur_core::orchestrator::InteractiveInput;
use spur_core::Orchestrator;
use spur_pm::test_workspace::TestBeadsWorkspace;
use tokio::sync::mpsc;

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Drain up to `limit` events from the broadcast receiver within `timeout`.
async fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<SpurEvent>,
    limit: usize,
    timeout: Duration,
) -> Vec<SpurEvent> {
    let mut events = Vec::with_capacity(limit);
    let deadline = tokio::time::Instant::now() + timeout;
    while events.len() < limit {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ev)) => events.push(ev),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
    events
}

// ── Test 1: no brain session ──────────────────────────────────────────────────

/// Build a minimal orchestrator (empty agent registry; no pm_service) and return
/// the ingress sender + event broadcast receiver.  Background `run_interactive`
/// exits when the sender is dropped.
fn build_orchestrator() -> (
    mpsc::Sender<InteractiveInput>,
    tokio::sync::broadcast::Receiver<SpurEvent>,
) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let orch = Orchestrator::new(tmp.path().into(), SpurConfig::default(), None)
        .expect("Orchestrator::new");

    let events_rx = orch.event_tx.subscribe();
    let (input_tx, input_rx) = mpsc::channel::<InteractiveInput>(16);
    let overflow = new_overflow_buf();

    tokio::spawn(async move {
        let _ = orch
            .run_interactive(
                input_rx, None, // brain_override
                None, // permission_tx
                overflow,
            )
            .await;
        drop(tmp);
    });

    (input_tx, events_rx)
}

fn attach_beads_workspace(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

fn init_git_repo(repo: &Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command failed");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    git(&["init", "-q"]);
    git(&["config", "user.email", "test@spur"]);
    git(&["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("README.md"), "test\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "seed"]);
}

async fn seed_loop_issue(pm: &spur_pm::PmService, loop_id: &str) -> String {
    let body = format!(
        "{}\n{}",
        spur_core::plan::loops::spec::SENTINEL_HEADER,
        json!({
            "loop_id": loop_id,
            "goal": "Keep CI green",
            "pattern": "ci-sweeper",
            "cadence_secs": 60,
            "template": {
                "tasks": [{
                    "task_id": "triage",
                    "agent": "codex",
                    "task": "Triage CI",
                    "labels": [spur_core::plan::labels::LOOP_TRIAGE_TASK]
                }]
            }
        })
    );

    pm.create_issue(spur_pm::IssueCreate {
        title: "Loop: Keep CI green".to_string(),
        description: Some(body),
        issue_type: Some("task".to_string()),
        labels: vec![spur_core::plan::labels::loop_id_label(loop_id)],
        ..Default::default()
    })
    .await
    .expect("create loop issue")
}

async fn build_orchestrator_with_loop(
    loop_id: &str,
) -> (
    mpsc::Sender<InteractiveInput>,
    tokio::sync::broadcast::Receiver<SpurEvent>,
) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_root = tmp.path().to_path_buf();
    init_git_repo(&repo_root);

    let beads = TestBeadsWorkspace::init();
    attach_beads_workspace(&repo_root, &beads);

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, &repo_root, None)
            .await
            .expect("PmService::try_new")
            .expect("expected Some(PmService)"),
    );
    seed_loop_issue(&pm, loop_id).await;

    let orch = Orchestrator::new(repo_root, SpurConfig::default(), None)
        .expect("Orchestrator::new")
        .with_pm_service(Arc::clone(&pm));
    let events_rx = orch.event_tx.subscribe();
    let (input_tx, input_rx) = mpsc::channel::<InteractiveInput>(16);
    let overflow = new_overflow_buf();

    tokio::spawn(async move {
        let _ = orch.run_interactive(input_rx, None, None, overflow).await;
        drop(tmp);
    });

    (input_tx, events_rx)
}

/// When `ResumePlan` is sent without any active brain session, the orchestrator
/// must emit exactly one `PlanCommandError` with the sentinel message.
#[tokio::test]
async fn no_brain_session_emits_plan_command_error() {
    let plan_id = "plan-resume-no-brain";

    let (input_tx, mut events_rx) = build_orchestrator();

    // Allow the orchestrator startup refresh to settle before sending input.
    tokio::task::yield_now().await;

    input_tx
        .send(InteractiveInput::ResumePlan {
            plan_id: plan_id.to_string(),
        })
        .await
        .expect("send ResumePlan");

    // Drain events for up to 3 seconds looking for PlanCommandError.
    let events = drain_events(&mut events_rx, 32, Duration::from_secs(3)).await;

    let matching: Vec<_> = events
        .iter()
        .filter(|ev| {
            matches!(
                &ev.body,
                SpurEventBody::PlanCommandError {
                    operation,
                    plan_id: Some(pid),
                    error,
                }
                if operation == "ResumePlan"
                    && pid.as_str() == plan_id
                    && error == "No active brain session — start one to resume plans"
            )
        })
        .collect();

    assert_eq!(
        matching.len(),
        1,
        "expected exactly one PlanCommandError(ResumePlan / 'No active brain session…') \
         but found {}. All events: {:#?}",
        matching.len(),
        events.iter().map(|e| &e.body).collect::<Vec<_>>(),
    );
}

/// When `RetryPlanTask` is sent without any active brain session, the
/// orchestrator must emit exactly one `PlanCommandError` with the supplied
/// optional plan id preserved for UI correlation.
#[tokio::test]
async fn retry_plan_task_no_brain_session_emits_plan_command_error() {
    let plan_id = "plan-retry-no-brain";
    let issue_id = "bd-retry-1";

    let (input_tx, mut events_rx) = build_orchestrator();

    tokio::task::yield_now().await;

    input_tx
        .send(InteractiveInput::RetryPlanTask {
            plan_id: Some(plan_id.to_string()),
            issue_id: issue_id.to_string(),
        })
        .await
        .expect("send RetryPlanTask");

    let events = drain_events(&mut events_rx, 32, Duration::from_secs(3)).await;

    let matching: Vec<_> = events
        .iter()
        .filter(|ev| {
            matches!(
                &ev.body,
                SpurEventBody::PlanCommandError {
                    operation,
                    plan_id: Some(pid),
                    error,
                }
                if operation == "RetryPlanTask"
                    && pid.as_str() == plan_id
                    && error == "No active brain session — start one to retry plan tasks"
            )
        })
        .collect();

    assert_eq!(
        matching.len(),
        1,
        "expected exactly one PlanCommandError(RetryPlanTask / 'No active brain session…') \
         but found {}. All events: {:#?}",
        matching.len(),
        events.iter().map(|e| &e.body).collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn refresh_loops_emits_loops_loaded_without_brain_session() {
    let loop_id = "looprefreshnobrain";
    let (input_tx, mut events_rx) = build_orchestrator_with_loop(loop_id).await;

    tokio::task::yield_now().await;

    input_tx
        .send(InteractiveInput::RefreshLoops)
        .await
        .expect("send RefreshLoops");

    let events = drain_events(&mut events_rx, 64, Duration::from_secs(3)).await;
    let loaded = events.iter().find_map(|ev| match &ev.body {
        SpurEventBody::LoopsLoaded { loops, warnings } => Some((loops, warnings)),
        _ => None,
    });

    let (loops, _warnings) = loaded.unwrap_or_else(|| {
        panic!(
            "expected LoopsLoaded after RefreshLoops. All events: {:#?}",
            events.iter().map(|e| &e.body).collect::<Vec<_>>()
        )
    });

    assert!(
        loops
            .iter()
            .any(|loop_summary| loop_summary.loop_id == loop_id
                && loop_summary.title == "Loop: Keep CI green"),
        "expected seeded loop in LoopsLoaded, got {loops:#?}"
    );
}

#[tokio::test]
async fn pause_loop_no_brain_session_emits_loop_command_error() {
    let loop_id = "looppausenobrain";
    let (input_tx, mut events_rx) = build_orchestrator();

    tokio::task::yield_now().await;

    input_tx
        .send(InteractiveInput::PauseLoop {
            loop_id: loop_id.to_string(),
        })
        .await
        .expect("send PauseLoop");

    let events = drain_events(&mut events_rx, 32, Duration::from_secs(3)).await;
    let matching: Vec<_> = events
        .iter()
        .filter(|ev| {
            matches!(
                &ev.body,
                SpurEventBody::LoopCommandError {
                    operation,
                    loop_id: Some(event_loop_id),
                    error,
                }
                if operation == "PauseLoop"
                    && event_loop_id == loop_id
                    && error == "No active brain session - start one to claim plans"
            )
        })
        .collect();

    assert_eq!(
        matching.len(),
        1,
        "expected exactly one LoopCommandError(PauseLoop / no brain) but found {}. All events: {:#?}",
        matching.len(),
        events.iter().map(|e| &e.body).collect::<Vec<_>>(),
    );
}

// ── Test 2: MCP server, plan owned by a different brain ───────────────────────
//
// This test exercises `McpCallbackServer::call_resume_plan` — the exact method
// the orchestrator dispatches to — with a real beads PM service.  The orchestrator
// loop itself cannot be seeded with an injected `BrainSession`; driving the
// bridge at this level is the closest possible integration without a live ACP
// transport.
//
// The fixture initializes beads through TestBeadsWorkspace rather than the
// `br` CLI. PmService still exercises the production beads backend.

/// Set up a minimal git + beads repo, create an epic whose labels carry a
/// `spur:owner:<other-session>` token, then confirm that `call_resume_plan`
/// returns an Err containing the ownership-conflict message.
#[tokio::test]
async fn mcp_server_plan_owned_by_other_emits_plan_command_error() {
    // ── 1. Git init ──────────────────────────────────────────────────────────
    let dir = tempfile::TempDir::new().expect("tempdir");

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git command failed")
    };

    git(&["init", "-q"]);
    git(&["config", "user.email", "test@spur"]);
    git(&["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("README.md"), "test\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "seed"]);

    // ── 2. Beads init ────────────────────────────────────────────────────────
    let beads = TestBeadsWorkspace::init();
    attach_beads_workspace(dir.path(), &beads);

    // ── 3. Create PmService ──────────────────────────────────────────────────
    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);

    // ── 4. Create an epic owned by a *different* brain session ───────────────
    let plan_id = "plan-owned-by-other";
    let other_session = "7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a";

    let epic_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Epic for plan-owned-by-other".to_string(),
            issue_type: Some("epic".to_string()),
            labels: vec![
                spur_core::plan::labels::plan_id(plan_id),
                spur_core::plan::labels::plan_owner(other_session),
            ],
            ..Default::default()
        })
        .await
        .expect("create epic");

    eprintln!("created epic {epic_id} with plan_id={plan_id}, owner={other_session}");

    // ── 5. Create McpCallbackServer for current brain (different session) ────
    let current_session_id = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
    let brain_session_id: spur_acp::BrainSessionId = current_session_id.clone().into();

    let ctx = spur_core::server::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };

    let (server, _channel) = spur_core::McpCallbackServer::new(
        Some(&brain_session_id),
        Some(Arc::clone(&pm)),
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_core::server::community_feature_gate(),
    );

    // ── 6. Call bridge method — must return Err with ownership message ────────
    let result = server.call_resume_plan(plan_id).await;

    assert!(
        result.is_err(),
        "call_resume_plan must return Err when plan is owned by a different brain; got Ok(())",
    );

    let error_msg = result.unwrap_err();
    let expected_owner = spur_core::plan::labels::compact_label_component(other_session);
    let expected_fragment = format!(
        "resume_plan: plan {plan_id} is owned by {expected_owner}; active handoff is not supported"
    );

    assert_eq!(
        error_msg, expected_fragment,
        "error message must match the verbatim ownership-conflict string from handle_resume_plan",
    );
}
