//! Phase 2e — `InsertTaskBefore`, `AddDependency`, `CancelTask` ops + the
//! `RetryExhaustedProposer` (v0 deterministic). Three failing-test specs
//! from the RCA's "Order of operations Phase 2e" — see
//! `docs/rca/2026-05-07-bd-2m2u-failed-no-auto-retry.md`.

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use spur_mcp::plan::mutation::{MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use uuid::Uuid;

mod common;

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn br_id(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

fn sentinels(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|c| audit_sentinel::parse_comment(&c.body))
        .filter_map(|r| r.ok())
        .collect()
}

async fn pm_for(dir: &TempDir) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new")
            .expect("expected beads pm"),
    )
}

fn task_draft(title: &str, description: &str) -> TaskDraft {
    serde_json::from_value(json!({
        "title": title,
        "description": description,
        "assignee": null,
        "priority": null,
    }))
    .expect("TaskDraft JSON must deserialize")
}

fn batch(mutation_id: Uuid, trigger: String, ops: Vec<PlanMutationOp>) -> MutationBatch {
    serde_json::from_value(json!({
        "mutation_id": mutation_id,
        "trigger_signal_id": null,
        "trigger_task_id": trigger,
        "ops": ops,
    }))
    .expect("MutationBatch JSON deserialize")
}

// ─── Test 1: InsertTaskBefore ────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_insert_task_before_creates_dep_edge_and_resets_target() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let target = br_id(
        &run_br(
            dir.path(),
            &["create", "Original target", "--silent", "-t", "task"],
        )
        .unwrap(),
    );

    let pm = pm_for(&dir).await;
    let original = pm.get_issue(&target).await.unwrap();
    let pre_blocked_by = original.blocked_by.clone();

    let b = batch(
        Uuid::new_v4(),
        target.clone(),
        vec![PlanMutationOp::InsertTaskBefore {
            target_issue_id: target.clone(),
            draft: task_draft(
                "Prerequisite cleanup",
                "Land cleanup before original target",
            ),
        }],
    );

    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect("insert_task_before apply");

    let after = pm.get_issue(&target).await.unwrap();
    let new_dep = after
        .blocked_by
        .iter()
        .find(|dep| !pre_blocked_by.iter().any(|prev| prev == *dep))
        .cloned()
        .expect("target must gain a new blocked_by edge to the inserted child");

    assert_ne!(
        after.status,
        pm.closed_status(),
        "target must be open (Pending-projecting) after insert"
    );

    let inserted = pm.get_issue(&new_dep).await.unwrap();
    assert_eq!(
        inserted.title, "Prerequisite cleanup",
        "inserted child carries draft title"
    );
    assert_ne!(
        inserted.status,
        pm.closed_status(),
        "inserted child must be open"
    );
}

// ─── Test 2: AddDependency rollback on post-hoc cycle detection ──────────

#[tokio::test]
async fn mutation_op_add_dependency_post_hoc_cycle_triggers_rollback() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let b_id = br_id(&run_br(dir.path(), &["create", "B", "--silent", "-t", "task"]).unwrap());
    // B depends on A. AddDependency(A, B) creates A → B → A cycle.
    run_br(dir.path(), &["dep", "add", &b_id, &a]).unwrap();

    let pm = pm_for(&dir).await;
    let pre_a = pm.get_issue(&a).await.unwrap();
    let pre_blocked_by = pre_a.blocked_by.clone();

    let bx = batch(
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::AddDependency {
            issue_id: a.clone(),
            depends_on: b_id.clone(),
        }],
    );

    let err = apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &bx)
        .await
        .expect_err("must fail on cycle");
    assert!(
        format!("{err:#}").contains("cycle"),
        "expected cycle error: {err:?}"
    );

    let restored = pm.get_issue(&a).await.unwrap();
    assert_eq!(
        restored.blocked_by, pre_blocked_by,
        "blocked_by must be restored after cycle rollback"
    );

    let adv = pm.advanced().expect("adv");
    let comments = adv.list_comments(&a).await.unwrap();
    let sents = sentinels(&comments);
    assert!(
        sents
            .iter()
            .any(|s| matches!(s, AuditSentinelKind::MutationInvariantViolation { .. })),
        "violation audit required"
    );
}

// ─── Test 3: CancelTask does not cascade ─────────────────────────────────

#[tokio::test]
async fn mutation_op_cancel_task_does_not_cascade_and_does_not_close_descendants() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let b_id = br_id(&run_br(dir.path(), &["create", "B", "--silent", "-t", "task"]).unwrap());
    let c = br_id(&run_br(dir.path(), &["create", "C", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["dep", "add", &b_id, &a]).unwrap();
    run_br(dir.path(), &["dep", "add", &c, &b_id]).unwrap();

    let pm = pm_for(&dir).await;
    let bx = batch(
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::CancelTask {
            issue_id: a.clone(),
            reason: "work no longer needed".into(),
        }],
    );

    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &bx)
        .await
        .expect("cancel apply");

    let a_issue = pm.get_issue(&a).await.unwrap();
    assert_eq!(
        a_issue.status,
        pm.closed_status(),
        "A must be closed (cancelled) after CancelTask"
    );

    let b_issue = pm.get_issue(&b_id).await.unwrap();
    assert_ne!(
        b_issue.status,
        pm.closed_status(),
        "B must remain open: CancelTask must not cascade"
    );
    let c_issue = pm.get_issue(&c).await.unwrap();
    assert_ne!(
        c_issue.status,
        pm.closed_status(),
        "C must remain open: CancelTask must not cascade transitively"
    );

    let adv = pm.advanced().expect("adv");
    let comments = adv.list_comments(&a).await.unwrap();
    let sents = sentinels(&comments);
    assert!(
        sents.iter().any(|s| matches!(
            s,
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Cancelled,
                ..
            }
        )),
        "Completion(Cancelled) audit required to distinguish from Failed"
    );
    assert!(
        !sents.iter().any(|s| matches!(
            s,
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Failed,
                ..
            }
        )),
        "no Completion(Failed) — CancelTask must not look like AbandonTask"
    );
}

// ─── Test 4: AddDependency self-reference rejected before any write ──────

#[tokio::test]
async fn add_dependency_self_reference_rejected_early() {
    // bd-2m2u Phase 2e — E3 guard. `apply_add_dependency` must reject
    // `issue_id == depends_on` up front rather than relying on the post-hoc
    // `dep_cycles_with_fallback` scan to surface the trivial self-loop. The
    // post-hoc path does eventually catch it, but only after a full apply +
    // rollback round-trip; the early guard keeps the error path cheap and
    // the produced message specific to the typo.
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());

    let pm = pm_for(&dir).await;
    let pre_a = pm.get_issue(&a).await.unwrap();
    let pre_blocked_by = pre_a.blocked_by.clone();

    let bx = batch(
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::AddDependency {
            issue_id: a.clone(),
            depends_on: a.clone(),
        }],
    );

    let err = apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &bx)
        .await
        .expect_err("must fail on self-reference");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("self-reference") || msg.contains("cannot depend on itself"),
        "expected self-reference rejection message; got: {msg}"
    );

    // The early guard returns before any pm.add_dependency call, so the
    // pre-image must be byte-for-byte identical (no rollback round-trip).
    let after = pm.get_issue(&a).await.unwrap();
    assert_eq!(
        after.blocked_by, pre_blocked_by,
        "self-reference must not mutate blocked_by even transiently"
    );
}
