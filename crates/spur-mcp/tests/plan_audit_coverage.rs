//! Integration test: verify that [[spur-audit v1]] sentinel comments are
//! emitted at four plan-lifecycle transition points:
//!   1. PlanSubmit  — on the epic issue (via emit_plan_submit_audit)
//!   2. Dispatch    — on the task issue (emit_dispatch_audit)
//!   3. Completion  — on the task issue (emit_completion_audit)
//!   4. Approval    — on the task issue (via handle_review_task → approve)
//!
//! Test strategy: narrow emission-helper test (not full end-to-end executor),
//! because wiring a real DelegationResult through the plan executor in an
//! integration test would require standing up a mock orchestrator. Instead:
//!
//!   - Emit PlanSubmit via `emit_plan_submit_audit` (mirrors Task 5 test).
//!   - Emit Dispatch/Completion via their helpers directly.
//!   - Emit Approval via `handle_review_task` with a task already in
//!     AwaitingReview state.
//!
//! All four sentinels are validated by reading back `br comments list`.
//!
//! Requires `br` on PATH. Skipped (not failed) when `br` is unavailable.
//!
//! TODO(v0b): Add `run_plan_drives_dispatch_and_completion_emission` — a test
//! that wires a real `mpsc::Sender<DelegationRequest>` + matching rx, spawns a
//! task that echoes a canned `DelegationResult` back via the response oneshot,
//! calls `run_plan(...)`, waits for plan completion, then asserts Dispatch +
//! Completion sentinels appear in `br comments list`. Deferred because it
//! requires standing up a mock orchestrator in the test process, which is more
//! invasive than the narrow helper tests above.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_acp::{DelegationResult, DelegationStatus};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::tools::DelegationRequest;
use tempfile::TempDir;
use tokio::sync::Mutex;

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

/// Parse comments from a `br comments list` JSON output and collect only those
/// that are valid `[[spur-audit v1]]` sentinels.
fn collect_sentinels(list_json: &str) -> Vec<AuditSentinelKind> {
    let items: serde_json::Value =
        serde_json::from_str(list_json).expect("br comments list must be valid JSON");
    items
        .as_array()
        .expect("comments must be JSON array")
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(|r| r.ok())
        .collect()
}

#[tokio::test]
async fn plan_audit_coverage_all_four_sentinels() {
    if !br_available() {
        eprintln!("skipping plan_audit_coverage_all_four_sentinels: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService)");

    let tasks = vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Do the thing.".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }];

    // Build epic subgraph — creates epic + child issue in beads.
    let subgraph =
        spur_mcp::build_epic_subgraph(&pm, "audit-plan-1", "Audit Coverage Epic", None, &tasks)
            .await
            .expect("build_epic_subgraph must succeed");

    let task_issue_id = subgraph
        .task_map
        .get("t1")
        .expect("t1 must be in task_map")
        .clone();
    let epic_issue_id = subgraph.epic_id.clone();

    // ── 1. PlanSubmit — on epic issue ───────────────────────────────────────
    let adv = pm.advanced().expect("beads backend must have advanced()");
    spur_mcp::emit_plan_submit_audit(adv, "audit-plan-1", &subgraph).await;

    // ── 2. Dispatch — on task issue ─────────────────────────────────────────
    let delegation_id = "del-audit-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = Arc::new(pm);

    let issue_id_opt = Some(task_issue_id.clone());
    spur_mcp::plan::emit_dispatch_audit(
        Some(pm_arc.as_ref()),
        &issue_id_opt,
        "audit-plan-1",
        &delegation_id,
        "codex",
        1,
    )
    .await;

    // ── 3. Completion — on task issue ────────────────────────────────────────
    spur_mcp::plan::emit_completion_audit(
        Some(pm_arc.as_ref()),
        &issue_id_opt,
        "audit-plan-1",
        &delegation_id,
        Some("feat/worker-branch-1"),
        Some("3 files changed"),
    )
    .await;

    // ── 4. Approval — via handle_review_task ────────────────────────────────
    let entry = PlanTaskEntry {
        spec: PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Do the thing.".into(),
            depends_on: vec![],
            issue_id: Some(task_issue_id.clone()),
            context_files: vec![],
        },
        status: PlanTaskStatus::AwaitingReview {
            summary: Some("looks good".into()),
        },
        result: None,
        worker_branch: Some("feat/worker-branch-1".into()),
        attempt: 1,
        history: vec![],
        last_delegation_id: Some(delegation_id.clone()),
    };
    let plan_state = PlanState {
        plan_id: "audit-plan-1".into(),
        tasks: vec![entry],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        epic_id: Some(epic_issue_id.clone()),
    };
    let plan_arc_state = Arc::new(Mutex::new(plan_state));

    spur_mcp::plan::handle_review_task(
        plan_arc_state,
        "audit-plan-1",
        "t1",
        "approve",
        Some("all good"),
        Some(pm_arc.clone()),
        None,
        None,
        None,
    )
    .await
    .expect("handle_review_task must succeed");

    // ── Assertions ───────────────────────────────────────────────────────────

    // Epic: PlanSubmit sentinel.
    let epic_comments =
        run_br(dir.path(), &["comments", "list", &epic_issue_id]).expect("br comments list epic");
    let epic_sentinels = collect_sentinels(&epic_comments);
    let plan_submit_found = epic_sentinels.iter().any(
        |k| matches!(k, AuditSentinelKind::PlanSubmit { plan_id, .. } if plan_id == "audit-plan-1"),
    );
    assert!(
        plan_submit_found,
        "PlanSubmit sentinel must be on epic {epic_issue_id}; got: {epic_sentinels:?}"
    );

    // Task: Dispatch → Completion → Approval in order.
    let task_comments =
        run_br(dir.path(), &["comments", "list", &task_issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);

    let dispatch_pos = task_sentinels.iter().position(|k| {
        matches!(k, AuditSentinelKind::Dispatch { delegation_id: did, .. } if did == "del-audit-001")
    });
    let completion_pos = task_sentinels.iter().position(|k| {
        matches!(k, AuditSentinelKind::Completion { delegation_id: did, .. } if did == "del-audit-001")
    });
    let approval_pos = task_sentinels.iter().position(|k| {
        matches!(k, AuditSentinelKind::Approval { delegation_id: did } if did == "del-audit-001")
    });

    assert!(
        dispatch_pos.is_some(),
        "Dispatch sentinel must be on task {task_issue_id}; got: {task_sentinels:?}"
    );
    assert!(
        completion_pos.is_some(),
        "Completion sentinel must be on task {task_issue_id}; got: {task_sentinels:?}"
    );
    assert!(
        approval_pos.is_some(),
        "Approval sentinel must be on task {task_issue_id}; got: {task_sentinels:?}"
    );

    // Verify ordering: Dispatch < Completion < Approval.
    let dp = dispatch_pos.unwrap();
    let cp = completion_pos.unwrap();
    let ap = approval_pos.unwrap();
    assert!(
        dp < cp,
        "Dispatch must precede Completion (positions: {dp} vs {cp})"
    );
    assert!(
        cp < ap,
        "Completion must precede Approval (positions: {cp} vs {ap})"
    );
}

#[tokio::test]
async fn rejection_emits_rejection_sentinel() {
    if !br_available() {
        eprintln!("skipping rejection_emits_rejection_sentinel: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService)");

    let tasks = vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Do the rejection thing.".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }];

    // Build epic subgraph — creates epic + child issue in beads.
    let subgraph =
        spur_mcp::build_epic_subgraph(&pm, "audit-reject-1", "Rejection Audit Epic", None, &tasks)
            .await
            .expect("build_epic_subgraph must succeed");

    let task_issue_id = subgraph
        .task_map
        .get("t1")
        .expect("t1 must be in task_map")
        .clone();

    let delegation_id = "del-reject-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = Arc::new(pm);

    // Build task entry already in AwaitingReview state (as if a worker completed).
    let entry = PlanTaskEntry {
        spec: PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Do the rejection thing.".into(),
            depends_on: vec![],
            issue_id: Some(task_issue_id.clone()),
            context_files: vec![],
        },
        status: PlanTaskStatus::AwaitingReview {
            summary: Some("worker done".into()),
        },
        result: None,
        worker_branch: Some("feat/worker-branch-rej".into()),
        attempt: 1,
        history: vec![],
        last_delegation_id: Some(delegation_id.clone()),
    };
    let plan_state = PlanState {
        plan_id: "audit-reject-1".into(),
        tasks: vec![entry],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        epic_id: Some(subgraph.epic_id.clone()),
    };
    let plan_arc_state = Arc::new(Mutex::new(plan_state));

    // Call handle_review_task with decision=reject and feedback.
    spur_mcp::plan::handle_review_task(
        plan_arc_state,
        "audit-reject-1",
        "t1",
        "reject",
        Some("needs more tests"),
        Some(pm_arc.clone()),
        None,
        None,
        None,
    )
    .await
    .expect("handle_review_task must succeed");

    // Fetch comments and assert Rejection sentinel is present.
    let task_comments =
        run_br(dir.path(), &["comments", "list", &task_issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);

    let rejection_found = task_sentinels.iter().any(|k| {
        matches!(
            k,
            AuditSentinelKind::Rejection { delegation_id: did, feedback }
                if did == "del-reject-001" && feedback == "needs more tests"
        )
    });
    assert!(
        rejection_found,
        "Rejection sentinel must be on task {task_issue_id} with delegation_id=del-reject-001 \
         and feedback='needs more tests'; got: {task_sentinels:?}"
    );
}

/// Bug 1 regression guard: request_changes → re-dispatch → completion must
/// emit a Completion sentinel for the SECOND attempt. Before this fix,
/// `spawn_completion_future` (the path used by every non-primary dispatcher)
/// dropped the audit emission silently.
#[tokio::test]
async fn request_changes_redispatch_emits_completion_sentinel() {
    if !br_available() {
        eprintln!(
            "skipping request_changes_redispatch_emits_completion_sentinel: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let tasks = vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Do the redispatch thing.".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }];

    let subgraph =
        spur_mcp::build_epic_subgraph(&pm, "audit-redis-1", "Redispatch Audit Epic", None, &tasks)
            .await
            .expect("build_epic_subgraph must succeed");

    let task_issue_id = subgraph
        .task_map
        .get("t1")
        .expect("t1 must be in task_map")
        .clone();

    let initial_delegation_id = "del-initial-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = Arc::new(pm);

    // Task already AwaitingReview on attempt 1 — as if a worker finished once.
    let entry = PlanTaskEntry {
        spec: PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Do the redispatch thing.".into(),
            depends_on: vec![],
            issue_id: Some(task_issue_id.clone()),
            context_files: vec![],
        },
        status: PlanTaskStatus::AwaitingReview {
            summary: Some("first try narrative".into()),
        },
        result: None,
        worker_branch: Some("feat/v1".into()),
        attempt: 1,
        history: vec![],
        last_delegation_id: Some(initial_delegation_id.clone()),
    };
    let plan_state = PlanState {
        plan_id: "audit-redis-1".into(),
        tasks: vec![entry],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        epic_id: Some(subgraph.epic_id.clone()),
    };
    let plan_arc_state = Arc::new(Mutex::new(plan_state));

    // Capture the re-dispatched DelegationRequest so we can respond with Success.
    let (del_tx, mut del_rx) = tokio::sync::mpsc::channel::<DelegationRequest>(4);
    let tracker = tokio_util::task::TaskTracker::new();

    // Background: when the re-dispatch fires, respond with Success and capture
    // the new delegation_id.
    let worker = tokio::spawn(async move {
        let req = del_rx
            .recv()
            .await
            .expect("re-dispatch must enqueue a DelegationRequest");
        let new_id = req.id.to_string();
        let _ = req.respond_to.send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("second attempt narrative".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("feat/v2".into()),
            artifact: None,
        });
        new_id
    });

    spur_mcp::plan::handle_review_task(
        plan_arc_state.clone(),
        "audit-redis-1",
        "t1",
        "request_changes",
        Some("please revise"),
        Some(pm_arc.clone()),
        None,
        Some(&del_tx),
        Some(&tracker),
    )
    .await
    .expect("handle_review_task must succeed");

    let new_delegation_id = worker.await.expect("worker join");

    // Wait for the spawned completion future to finish emitting.
    tracker.close();
    tracker.wait().await;

    let task_comments =
        run_br(dir.path(), &["comments", "list", &task_issue_id]).expect("br comments list task");
    let sentinels = collect_sentinels(&task_comments);

    let completion_found = sentinels.iter().any(|k| {
        matches!(
            k,
            AuditSentinelKind::Completion { delegation_id, result_summary, worker_branch }
                if delegation_id == &new_delegation_id
                    && result_summary.as_deref() == Some("second attempt narrative")
                    && worker_branch.as_deref() == Some("feat/v2")
        )
    });
    assert!(
        completion_found,
        "Completion sentinel for re-dispatched delegation_id={new_delegation_id} must be present \
         on task {task_issue_id}; got: {sentinels:?}"
    );
}

/// Approval-cascade regression: approving a task whose children are Pending
/// triggers `dispatch_newly_ready` → `spawn_completion_future` for each newly
/// unblocked task. When those cascade-dispatched tasks complete, the
/// `spawn_completion_future` path must emit a Completion sentinel too.
///
/// Before Fix A (commit 8e759c0), spawn_completion_future dropped audit
/// emission silently regardless of which dispatcher invoked it. This test
/// exercises the approval-cascade dispatcher specifically, complementing
/// `request_changes_redispatch_emits_completion_sentinel` which covers the
/// rejection-re-dispatch dispatcher.
#[tokio::test]
async fn approval_cascade_dispatched_task_emits_completion_sentinel() {
    if !br_available() {
        eprintln!(
            "skipping approval_cascade_dispatched_task_emits_completion_sentinel: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let tasks = vec![
        PlanTask {
            task_id: "a".into(),
            agent: "codex".into(),
            task: "First task.".into(),
            depends_on: vec![],
            issue_id: None,
            context_files: vec![],
        },
        PlanTask {
            task_id: "b".into(),
            agent: "codex".into(),
            task: "Second task, depends on a.".into(),
            depends_on: vec!["a".into()],
            issue_id: None,
            context_files: vec![],
        },
    ];

    let subgraph =
        spur_mcp::build_epic_subgraph(&pm, "audit-cascade-1", "Cascade Audit Epic", None, &tasks)
            .await
            .expect("build_epic_subgraph must succeed");

    let task_a_id = subgraph.task_map.get("a").expect("a").clone();
    let task_b_id = subgraph.task_map.get("b").expect("b").clone();

    let a_delegation_id = "del-a-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = Arc::new(pm);

    // Stage: A is AwaitingReview (worker finished), B is still Pending blocked on A.
    let entry_a = PlanTaskEntry {
        spec: PlanTask {
            task_id: "a".into(),
            agent: "codex".into(),
            task: "First task.".into(),
            depends_on: vec![],
            issue_id: Some(task_a_id.clone()),
            context_files: vec![],
        },
        status: PlanTaskStatus::AwaitingReview {
            summary: Some("a narrative".into()),
        },
        result: None,
        worker_branch: Some("feat/a".into()),
        attempt: 1,
        history: vec![],
        last_delegation_id: Some(a_delegation_id.clone()),
    };
    let entry_b = PlanTaskEntry {
        spec: PlanTask {
            task_id: "b".into(),
            agent: "codex".into(),
            task: "Second task, depends on a.".into(),
            depends_on: vec!["a".into()],
            issue_id: Some(task_b_id.clone()),
            context_files: vec![],
        },
        status: PlanTaskStatus::Pending,
        result: None,
        worker_branch: None,
        attempt: 1,
        history: vec![],
        last_delegation_id: None,
    };
    let plan_state = PlanState {
        plan_id: "audit-cascade-1".into(),
        tasks: vec![entry_a, entry_b],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        epic_id: Some(subgraph.epic_id.clone()),
    };
    let plan_arc_state = Arc::new(Mutex::new(plan_state));

    // When the cascade dispatches B, respond Success on its oneshot.
    let (del_tx, mut del_rx) = tokio::sync::mpsc::channel::<DelegationRequest>(4);
    let tracker = tokio_util::task::TaskTracker::new();

    let worker = tokio::spawn(async move {
        let req = del_rx
            .recv()
            .await
            .expect("approval cascade must enqueue a DelegationRequest for task b");
        let b_new_id = req.id.to_string();
        let _ = req.respond_to.send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("b narrative".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("feat/b".into()),
            artifact: None,
        });
        b_new_id
    });

    spur_mcp::plan::handle_review_task(
        plan_arc_state.clone(),
        "audit-cascade-1",
        "a",
        "approve",
        None,
        Some(pm_arc.clone()),
        None,
        Some(&del_tx),
        Some(&tracker),
    )
    .await
    .expect("handle_review_task approve must succeed");

    let b_delegation_id = worker.await.expect("worker join");

    tracker.close();
    tracker.wait().await;

    // Approval sentinel should be on task A.
    let a_comments =
        run_br(dir.path(), &["comments", "list", &task_a_id]).expect("br comments list a");
    let a_sentinels = collect_sentinels(&a_comments);
    assert!(
        a_sentinels.iter().any(|k| matches!(
            k,
            AuditSentinelKind::Approval { delegation_id } if delegation_id == &a_delegation_id
        )),
        "Approval sentinel for a's delegation_id={a_delegation_id} must be on task {task_a_id}; got {a_sentinels:?}"
    );

    // Completion sentinel for B should be on task B — emitted via spawn_completion_future
    // spawned from dispatch_newly_ready. This is the codex-acp REQUEST_CHANGES coverage gap.
    let b_comments =
        run_br(dir.path(), &["comments", "list", &task_b_id]).expect("br comments list b");
    let b_sentinels = collect_sentinels(&b_comments);
    assert!(
        b_sentinels.iter().any(|k| matches!(
            k,
            AuditSentinelKind::Completion { delegation_id, result_summary, worker_branch }
                if delegation_id == &b_delegation_id
                    && result_summary.as_deref() == Some("b narrative")
                    && worker_branch.as_deref() == Some("feat/b")
        )),
        "Completion sentinel for cascade-dispatched b (delegation_id={b_delegation_id}) \
         must be on task {task_b_id}; got {b_sentinels:?}"
    );

    // Also: Dispatch sentinel for B from the cascade path.
    assert!(
        b_sentinels.iter().any(|k| matches!(
            k,
            AuditSentinelKind::Dispatch { delegation_id, .. }
                if delegation_id == &b_delegation_id
        )),
        "Dispatch sentinel for cascade-dispatched b (delegation_id={b_delegation_id}) \
         must be on task {task_b_id}; got {b_sentinels:?}"
    );
}
