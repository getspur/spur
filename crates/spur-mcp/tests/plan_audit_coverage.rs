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

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
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
        Some(&pm_arc),
        &issue_id_opt,
        "audit-plan-1",
        &delegation_id,
        "codex",
        1,
    )
    .await;

    // ── 3. Completion — on task issue ────────────────────────────────────────
    spur_mcp::plan::emit_completion_audit(
        Some(&pm_arc),
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
        status: PlanTaskStatus::AwaitingReview { summary: Some("looks good".into()) },
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
        Some(pm_arc.as_ref()),
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
    let plan_submit_found = epic_sentinels.iter().any(|k| {
        matches!(k, AuditSentinelKind::PlanSubmit { plan_id, .. } if plan_id == "audit-plan-1")
    });
    assert!(
        plan_submit_found,
        "PlanSubmit sentinel must be on epic {epic_issue_id}; got: {epic_sentinels:?}"
    );

    // Task: Dispatch → Completion → Approval in order.
    let task_comments = run_br(dir.path(), &["comments", "list", &task_issue_id])
        .expect("br comments list task");
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
