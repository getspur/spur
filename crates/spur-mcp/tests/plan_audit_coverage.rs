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

use spur_mcp::plan::audit_sentinel::{
    self, AuditSentinelKind, CompletionAuditFields, CompletionState,
};
use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use tempfile::TempDir;
use tokio::sync::Mutex;

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

fn extract_id(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("br create json");
    value["id"].as_str().expect("br create id").to_string()
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

async fn add_labels_individually(pm: &spur_pm::PmService, issue_id: &str, labels: &[String]) {
    for label in labels {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .expect("seed label");
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");

    let issue_id = extract_id(
        &run_br(dir.path(), &["create", "Dispatch Target", "-t", "task"]).expect("create issue"),
    );

    spur_mcp::plan::persist_dispatch_intent(
        &pm,
        &issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-1",
        "del-A",
        "codex",
        1,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("persist dispatch intent");

    let issue = pm.get_issue(&issue_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .contains(&spur_mcp::plan::labels::delegation_id("del-A")),
        "dispatch label must be present after persistence: {:?}",
        issue.labels
    );
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
    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "audit-plan-1",
        "Audit Coverage Epic",
        None,
        &tasks,
    )
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
    spur_mcp::emit_plan_submit_audit(
        adv,
        "audit-plan-1",
        &subgraph,
        Some("spur/brain-snapshot-test"),
        Some("0123456789abcdef0123456789abcdef01234567"),
        None,
        Some(&spur_acp::SessionId("brain".into())),
    )
    .await;

    // ── 2. Dispatch — on task issue ─────────────────────────────────────────
    let delegation_id = "del-audit-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = Arc::new(pm);

    let issue_id_opt = Some(task_issue_id.clone());
    spur_mcp::plan::emit_dispatch_audit(
        Some(pm_arc.as_ref()),
        &issue_id_opt,
        common::server_builder::pro_feature_gate().as_ref(),
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
        common::server_builder::pro_feature_gate().as_ref(),
        "audit-plan-1",
        &delegation_id,
        CompletionState::AwaitingReview,
        false,
        CompletionAuditFields {
            worker_branch: Some("feat/worker-branch-1".into()),
            result_summary: Some("3 files changed".into()),
            ..Default::default()
        },
    )
    .await
    .expect("emit completion audit");

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
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
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
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("handle_review_task must succeed");

    // ── Assertions ───────────────────────────────────────────────────────────

    // Epic: PlanSubmit sentinel.
    let epic_comments =
        run_br(dir.path(), &["comments", "list", &epic_issue_id]).expect("br comments list epic");
    let epic_sentinels = collect_sentinels(&epic_comments);
    let plan_submit_found = epic_sentinels.iter().any(|k| {
        matches!(
            k,
            AuditSentinelKind::PlanSubmit {
                plan_id,
                base_snapshot_branch: Some(base_snapshot_branch),
                base_snapshot_oid: Some(base_snapshot_oid),
                ..
            } if plan_id == "audit-plan-1"
                && base_snapshot_branch == "spur/brain-snapshot-test"
                && base_snapshot_oid == "0123456789abcdef0123456789abcdef01234567"
        )
    });
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
async fn epic_completion_audit_round_trips_through_collect_sentinels() {
    if !br_available() {
        eprintln!(
            "skipping epic_completion_audit_round_trips_through_collect_sentinels: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let epic_id = serde_json::from_str::<serde_json::Value>(
        &run_br(
            dir.path(),
            &["create", "Epic completion epic", "-t", "epic"],
        )
        .expect("create epic"),
    )
    .expect("create epic json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    let adv = pm.advanced().expect("beads backend must have advanced()");
    spur_mcp::plan::emit_epic_completion_audit(
        adv,
        &epic_id,
        "audit-plan-epic",
        spur_mcp::plan::audit_sentinel::EpicCompletionOutcome::AllApproved,
    )
    .await
    .expect("emit epic completion audit must succeed");

    let epic_comments =
        run_br(dir.path(), &["comments", "list", &epic_id]).expect("br comments list epic");
    let epic_sentinels = collect_sentinels(&epic_comments);
    assert!(epic_sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::EpicCompletion {
            outcome: spur_mcp::plan::audit_sentinel::EpicCompletionOutcome::AllApproved,
            plan_id,
            epic_id: found_epic_id,
        } if plan_id == "audit-plan-epic" && found_epic_id == &epic_id
    )));
}

#[tokio::test]
async fn dispatch_intent_includes_lease_label() {
    if !br_available() {
        eprintln!("skipping dispatch_intent_includes_lease_label: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Dispatch intent task".into(),
            description: Some("body".into()),
            issue_type: Some("task".into()),
            labels: vec![
                spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
                "ready-for-review".to_string(),
                "delegation-id:old-del".to_string(),
            ],
            ..Default::default()
        })
        .await
        .expect("create issue");
    let stale_lease = spur_mcp::plan::labels::lease_expires_at(1_700_000_000);
    run_br(dir.path(), &["label", "add", &issue_id, &stale_lease]).expect("add stale lease");

    spur_mcp::plan::persist_dispatch_intent(
        &pm,
        &issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-1",
        "del-A",
        "codex",
        1,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("persist dispatch intent");

    let issue = pm.get_issue(&issue_id).await.expect("get issue");
    assert!(issue
        .labels
        .contains(&spur_mcp::plan::labels::delegation_id("del-A")));
    assert!(issue.labels.iter().any(|label| {
        spur_mcp::plan::labels::parse_lease_expires_at(label).is_some() && label != &stale_lease
    }));
    assert!(!issue.labels.contains(&stale_lease));
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()));
    assert!(!issue.labels.contains(&"ready-for-review".to_string()));
}

#[tokio::test]
async fn completion_success_writes_ready_for_review_and_completion_audit() {
    if !br_available() {
        eprintln!(
            "skipping completion_success_writes_ready_for_review_and_completion_audit: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    spur_mcp::plan::persist_completion_result(
        &pm,
        &issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-1",
        "del-A",
        CompletionState::AwaitingReview,
        CompletionAuditFields {
            worker_branch: Some("feat/task".into()),
            result_summary: Some("worker finished cleanly".into()),
            ..Default::default()
        },
        false,
    )
    .await
    .expect("persist completion");

    let issue = pm.get_issue(&issue_id).await.expect("get issue");
    assert!(issue
        .labels
        .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()));

    let task_comments =
        run_br(dir.path(), &["comments", "list", &issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);
    assert!(task_sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            completion_state: CompletionState::AwaitingReview,
            ..
        }
    )));
}

#[tokio::test]
async fn completion_failed_closes_issue_and_emits_completion_audit() {
    if !br_available() {
        eprintln!(
            "skipping completion_failed_closes_issue_and_emits_completion_audit: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    add_labels_individually(
        &pm,
        &issue_id,
        &[
            spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
            spur_mcp::plan::labels::delegation_id("del-fail"),
            "delegation-id:del-fail".to_string(),
        ],
    )
    .await;

    spur_mcp::plan::persist_completion_result(
        &pm,
        &issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-1",
        "del-fail",
        CompletionState::Failed,
        CompletionAuditFields {
            result_summary: Some("worker failed".into()),
            ..Default::default()
        },
        false,
    )
    .await
    .expect("persist completion");

    let issue = pm.get_issue(&issue_id).await.expect("get issue");
    assert_eq!(issue.status, "closed");
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()));
    assert!(!issue.labels.contains(&"ready-for-review".to_string()));
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::delegation_id("del-fail")));
    assert!(!issue.labels.contains(&"delegation-id:del-fail".to_string()));

    let task_comments =
        run_br(dir.path(), &["comments", "list", &issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);
    assert!(task_sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            completion_state: CompletionState::Failed,
            delegation_id,
            ..
        } if delegation_id == "del-fail"
    )));
}

#[tokio::test]
async fn completion_cancelled_closes_issue_and_emits_completion_audit() {
    if !br_available() {
        eprintln!(
            "skipping completion_cancelled_closes_issue_and_emits_completion_audit: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    add_labels_individually(
        &pm,
        &issue_id,
        &[
            spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
            spur_mcp::plan::labels::delegation_id("del-cancel"),
            "delegation-id:del-cancel".to_string(),
        ],
    )
    .await;

    spur_mcp::plan::persist_completion_result(
        &pm,
        &issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-1",
        "del-cancel",
        CompletionState::Cancelled,
        CompletionAuditFields {
            result_summary: Some("worker cancelled".into()),
            ..Default::default()
        },
        false,
    )
    .await
    .expect("persist completion");

    let issue = pm.get_issue(&issue_id).await.expect("get issue");
    assert_eq!(issue.status, "closed");
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()));
    assert!(!issue.labels.contains(&"ready-for-review".to_string()));
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::delegation_id("del-cancel")));
    assert!(!issue
        .labels
        .contains(&"delegation-id:del-cancel".to_string()));

    let task_comments =
        run_br(dir.path(), &["comments", "list", &issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);
    assert!(task_sentinels.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            completion_state: CompletionState::Cancelled,
            delegation_id,
            ..
        } if delegation_id == "del-cancel"
    )));
}

#[tokio::test]
async fn reject_closes_issue_and_adds_review_rejected_label() {
    if !br_available() {
        eprintln!("skipping reject_closes_issue_and_adds_review_rejected_label: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = Arc::new(
        spur_pm::PmService::try_new(
            None,  // no github_repo
            true,  // beads_enabled
            false, // github_enabled
            dir.path(),
            None, // closed_status default
        )
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)"),
    );

    let tasks = vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Do the rejection thing.".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }];

    // Build epic subgraph — creates epic + child issue in beads.
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "audit-reject-1",
        "Rejection Audit Epic",
        None,
        &tasks,
    )
    .await
    .expect("build_epic_subgraph must succeed");

    let task_issue_id = subgraph
        .task_map
        .get("t1")
        .expect("t1 must be in task_map")
        .clone();

    let delegation_id = "del-reject-001".to_string();
    add_labels_individually(
        pm.as_ref(),
        &task_issue_id,
        &[
            spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
        ],
    )
    .await;

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();

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
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
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
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("handle_review_task must succeed");

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert_eq!(issue.status, "closed");
    assert!(issue
        .labels
        .contains(&spur_mcp::plan::labels::REVIEW_REJECTED.to_string()));
    assert!(!issue
        .labels
        .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()));
    assert!(!issue.labels.contains(&"ready-for-review".to_string()));

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

#[tokio::test]
async fn request_changes_leaves_issue_open_and_not_review_ready() {
    if !br_available() {
        eprintln!(
            "skipping request_changes_leaves_issue_open_and_not_review_ready: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    let task_issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    add_labels_individually(
        pm.as_ref(),
        &task_issue_id,
        &[
            spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
        ],
    )
    .await;

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();
    let plan_arc_state = Arc::new(Mutex::new(PlanState {
        plan_id: "audit-request-changes-1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Do the request changes thing.".into(),
                depends_on: vec![],
                issue_id: Some(task_issue_id.clone()),
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("request changes narrative".into()),
            },
            result: None,
            worker_branch: Some("feat/request-changes".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-request-001".into()),
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
    }));

    spur_mcp::plan::handle_review_task(
        plan_arc_state.clone(),
        "audit-request-changes-1",
        "t1",
        "request_changes",
        Some("fix the edge case"),
        Some(pm_arc.clone()),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("handle_review_task must succeed");

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert_eq!(issue.status, "open");
    assert!(
        !issue
            .labels
            .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()),
        "request_changes must clear namespaced ready-for-review label"
    );
    assert!(
        !issue.labels.contains(&"ready-for-review".to_string()),
        "request_changes must clear legacy ready-for-review label"
    );
}

#[tokio::test]
async fn request_changes_does_not_emit_dispatch_audit() {
    if !br_available() {
        eprintln!("skipping request_changes_does_not_emit_dispatch_audit: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    let task_issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    add_labels_individually(
        pm.as_ref(),
        &task_issue_id,
        &[spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()],
    )
    .await;

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();
    let plan_arc_state = Arc::new(Mutex::new(PlanState {
        plan_id: "audit-request-changes-2".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Do the request changes thing.".into(),
                depends_on: vec![],
                issue_id: Some(task_issue_id.clone()),
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("request changes narrative".into()),
            },
            result: None,
            worker_branch: Some("feat/request-changes".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-request-002".into()),
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
    }));

    spur_mcp::plan::handle_review_task(
        plan_arc_state,
        "audit-request-changes-2",
        "t1",
        "request_changes",
        Some("fix the edge case"),
        Some(pm_arc.clone()),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("handle_review_task must succeed");

    let task_comments =
        run_br(dir.path(), &["comments", "list", &task_issue_id]).expect("br comments list task");
    let sentinels = collect_sentinels(&task_comments);

    assert!(
        !sentinels
            .iter()
            .any(|sentinel| matches!(sentinel, AuditSentinelKind::Dispatch { .. })),
        "review-driven request_changes must not emit Dispatch audit comments; got: {sentinels:?}"
    );
}

#[tokio::test]
async fn approve_closes_issue_and_clears_ready_for_review() {
    if !br_available() {
        eprintln!("skipping approve_closes_issue_and_clears_ready_for_review: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );

    let task_issue_id = serde_json::from_str::<serde_json::Value>(
        &run_br(dir.path(), &["create", "Task", "-t", "task"]).unwrap(),
    )
    .expect("create issue json")
    .get("id")
    .and_then(|value| value.as_str())
    .expect("id field")
    .to_string();

    add_labels_individually(
        pm.as_ref(),
        &task_issue_id,
        &[
            spur_mcp::plan::labels::READY_FOR_REVIEW.to_string(),
            "ready-for-review".to_string(),
        ],
    )
    .await;

    let delegation_id = "del-approve-001".to_string();
    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();

    let plan_state = PlanState {
        plan_id: "audit-approve-1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Approve the task.".into(),
                depends_on: vec![],
                issue_id: Some(task_issue_id.clone()),
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("approve narrative".into()),
            },
            result: None,
            worker_branch: Some("feat/approve".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: Some(delegation_id.clone()),
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
    };
    let plan_arc_state = Arc::new(Mutex::new(plan_state));

    spur_mcp::plan::handle_review_task(
        plan_arc_state.clone(),
        "audit-approve-1",
        "t1",
        "approve",
        None,
        Some(pm_arc.clone()),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("handle_review_task approve must succeed");

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert_eq!(issue.status, "closed");
    assert!(
        !issue
            .labels
            .contains(&spur_mcp::plan::labels::READY_FOR_REVIEW.to_string()),
        "approve must clear namespaced ready-for-review label"
    );
    assert!(
        !issue.labels.contains(&"ready-for-review".to_string()),
        "approve must clear legacy ready-for-review label"
    );

    let task_comments =
        run_br(dir.path(), &["comments", "list", &task_issue_id]).expect("br comments list task");
    let task_sentinels = collect_sentinels(&task_comments);
    assert!(
        task_sentinels.iter().any(|k| matches!(
            k,
            AuditSentinelKind::Approval { delegation_id: did } if did == &delegation_id
        )),
        "Approval sentinel must be on task {task_issue_id}; got {task_sentinels:?}"
    );
}
