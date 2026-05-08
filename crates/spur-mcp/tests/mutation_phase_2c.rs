//! Phase 2c — `RetryTask`, `ModifyTaskSpec`, `AbandonTask` ops + the
//! `submit_plan_mutation` MCP entry. Eleven failing tests from the RCA's
//! "Order of operations Phase 2c" — see
//! `docs/rca/2026-05-07-bd-2m2u-failed-no-auto-retry.md`.

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use spur_mcp::plan::labels::{self};
use spur_mcp::plan::mutation::{MutationBatch, PlanMutationOp};
use spur_mcp::plan::mutation_executor::{apply_mutation, submit_plan_mutation};
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

async fn close_failed(pm: &spur_pm::PmService, id: &str) {
    pm.update_issue(
        id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close issue");
}

async fn emit_completion_failed(adv: &dyn spur_pm::BeadsAdvanced, id: &str) {
    let comment = audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
        delegation_id: "del-prior".into(),
        completion_state: CompletionState::Failed,
        superseded: false,
        worker_branch: Some("spur/worker-prior".into()),
        result_summary: Some("worker crashed".into()),
        artifact_uri: None,
        dispatched_base_oid: None,
    });
    adv.add_comment(id, &comment).await.expect("seed audit");
}

async fn pm_for(dir: &TempDir) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new")
            .expect("expected beads pm"),
    )
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

// ─── Test 1 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_retry_task_resets_to_pending_preserves_history() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let issue_id = br_id(
        &run_br(
            dir.path(),
            &["create", "Failed task", "--silent", "-t", "task"],
        )
        .unwrap(),
    );
    let pm = pm_for(&dir).await;
    let adv = pm.advanced().expect("adv");
    emit_completion_failed(adv, &issue_id).await;
    close_failed(pm.as_ref(), &issue_id).await;

    let b = batch(
        Uuid::new_v4(),
        issue_id.clone(),
        vec![PlanMutationOp::RetryTask {
            issue_id: issue_id.clone(),
        }],
    );

    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect("retry apply");

    let issue = pm.get_issue(&issue_id).await.unwrap();
    assert_ne!(
        issue.status,
        pm.closed_status(),
        "issue must be open after retry"
    );

    let comments = adv.list_comments(&issue_id).await.unwrap();
    let sents = sentinels(&comments);
    assert!(
        sents
            .iter()
            .any(|s| matches!(s, AuditSentinelKind::RetryRequested { .. })),
        "RetryRequested audit must be emitted"
    );
    assert!(
        sents.iter().any(|s| matches!(
            s,
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Failed,
                ..
            }
        )),
        "prior Completion(Failed) audit history must be preserved"
    );
}

// ─── Test 2 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_retry_task_rolls_back_on_executor_failure() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let target =
        br_id(&run_br(dir.path(), &["create", "Target", "--silent", "-t", "task"]).unwrap());
    let dep = br_id(&run_br(dir.path(), &["create", "Dep", "--silent", "-t", "task"]).unwrap());
    // dep depends_on target
    run_br(dir.path(), &["dep", "add", &dep, &target]).unwrap();

    let pm = pm_for(&dir).await;
    let adv = pm.advanced().expect("adv");
    emit_completion_failed(adv, &target).await;
    close_failed(pm.as_ref(), &target).await;

    // Second op induces a cycle by making target depend on `dep` (which already
    // depends on target).
    let b = batch(
        Uuid::new_v4(),
        target.clone(),
        vec![
            PlanMutationOp::RetryTask {
                issue_id: target.clone(),
            },
            PlanMutationOp::ModifyTaskSpec {
                issue_id: target.clone(),
                new_task: None,
                new_agent: None,
                new_context_files: None,
                new_depends_on: Some(vec![dep.clone()]),
            },
        ],
    );

    let err = apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect_err("must fail on cycle");
    assert!(
        format!("{err:#}").contains("cycle"),
        "expected cycle error: {err:?}"
    );

    let issue = pm.get_issue(&target).await.unwrap();
    assert_eq!(
        issue.status,
        pm.closed_status(),
        "rollback must restore target to closed status"
    );

    let comments = adv.list_comments(&target).await.unwrap();
    let sents = sentinels(&comments);
    assert!(
        sents
            .iter()
            .any(|s| matches!(s, AuditSentinelKind::MutationInvariantViolation { .. })),
        "violation audit required"
    );
}

// ─── Test 3 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_modify_task_spec_updates_issue_body_labels_and_blocked_by() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let target = br_id(
        &run_br(
            dir.path(),
            &["create", "Original title", "--silent", "-t", "task"],
        )
        .unwrap(),
    );
    let old_dep =
        br_id(&run_br(dir.path(), &["create", "OldDep", "--silent", "-t", "task"]).unwrap());
    let new_dep =
        br_id(&run_br(dir.path(), &["create", "NewDep", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["dep", "add", &target, &old_dep]).unwrap();
    run_br(
        dir.path(),
        &["label", "add", &target, "-l", &labels::agent("codex")],
    )
    .unwrap();

    let pm = pm_for(&dir).await;

    let b = batch(
        Uuid::new_v4(),
        target.clone(),
        vec![PlanMutationOp::ModifyTaskSpec {
            issue_id: target.clone(),
            new_task: Some("New body content".into()),
            new_agent: Some("claude-code-acp".into()),
            new_context_files: Some(vec!["src/lib.rs".into()]),
            new_depends_on: Some(vec![new_dep.clone()]),
        }],
    );

    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect("modify apply");

    let issue = pm.get_issue(&target).await.unwrap();
    assert_eq!(issue.body, "New body content", "body must be updated");
    assert!(
        issue
            .labels
            .iter()
            .any(|l| l == &labels::agent("claude-code-acp")),
        "new agent label must be present, labels={:?}",
        issue.labels
    );
    assert!(
        !issue.labels.iter().any(|l| l == &labels::agent("codex")),
        "old agent label must be removed, labels={:?}",
        issue.labels
    );
    assert!(
        issue.blocked_by.iter().any(|d| d == &new_dep),
        "new dep must be present, blocked_by={:?}",
        issue.blocked_by
    );
    assert!(
        !issue.blocked_by.iter().any(|d| d == &old_dep),
        "old dep must be removed, blocked_by={:?}",
        issue.blocked_by
    );
}

// ─── Test 4 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_modify_task_spec_emits_extended_taskspec_audit() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let target = br_id(&run_br(dir.path(), &["create", "T", "--silent", "-t", "task"]).unwrap());
    let pm = pm_for(&dir).await;
    let adv = pm.advanced().unwrap();

    let b = batch(
        Uuid::new_v4(),
        target.clone(),
        vec![PlanMutationOp::ModifyTaskSpec {
            issue_id: target.clone(),
            new_task: Some("rewritten task".into()),
            new_agent: Some("claude-code-acp".into()),
            new_context_files: Some(vec!["docs/spec.md".into()]),
            new_depends_on: Some(vec![]),
        }],
    );

    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect("apply");

    let comments = adv.list_comments(&target).await.unwrap();
    let sents = sentinels(&comments);
    let extended = sents
        .iter()
        .find_map(|s| match s {
            AuditSentinelKind::TaskSpec {
                task_text,
                agent,
                depends_on,
                context_files,
                ..
            } => Some((
                task_text.clone(),
                agent.clone(),
                depends_on.clone(),
                context_files.clone(),
            )),
            _ => None,
        })
        .expect("extended TaskSpec audit must be emitted");
    assert_eq!(extended.0.as_deref(), Some("rewritten task"));
    assert_eq!(extended.1.as_deref(), Some("claude-code-acp"));
    assert_eq!(extended.2.as_ref().map(Vec::as_slice), Some(&[][..]));
    assert_eq!(extended.3, vec!["docs/spec.md".to_string()]);
}

// ─── Test 4b ─────────────────────────────────────────────────────────────
// Backwards compat: legacy TaskSpec data (no extended fields) still parses.

#[test]
fn legacy_taskspec_audit_round_trips_without_extended_fields() {
    let json = r#"{"kind":"task-spec","task_id":"bd-1","context_files":["src/a.rs"]}"#;
    let parsed: AuditSentinelKind = serde_json::from_str(json).expect("legacy parses");
    match parsed {
        AuditSentinelKind::TaskSpec {
            task_id,
            context_files,
            task_text,
            agent,
            depends_on,
        } => {
            assert_eq!(task_id, "bd-1");
            assert_eq!(context_files, vec!["src/a.rs".to_string()]);
            assert!(task_text.is_none());
            assert!(agent.is_none());
            assert!(depends_on.is_none());
        }
        other => panic!("expected TaskSpec, got {other:?}"),
    }
}

// ─── Test 5 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_modify_task_spec_rolls_back_to_prior_spec() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let target = br_id(&run_br(dir.path(), &["create", "T", "--silent", "-t", "task"]).unwrap());
    let dep = br_id(&run_br(dir.path(), &["create", "Dep", "--silent", "-t", "task"]).unwrap());
    // dep depends_on target — modifying target to depend on dep creates cycle.
    run_br(dir.path(), &["dep", "add", &dep, &target]).unwrap();
    run_br(
        dir.path(),
        &["label", "add", &target, "-l", &labels::agent("codex")],
    )
    .unwrap();
    let pm = pm_for(&dir).await;

    let original = pm.get_issue(&target).await.unwrap();
    let original_body = original.body.clone();
    let original_blocked_by = original.blocked_by.clone();

    let b = batch(
        Uuid::new_v4(),
        target.clone(),
        vec![PlanMutationOp::ModifyTaskSpec {
            issue_id: target.clone(),
            new_task: Some("new body".into()),
            new_agent: Some("claude-code-acp".into()),
            new_context_files: None,
            new_depends_on: Some(vec![dep.clone()]),
        }],
    );

    let err = apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &b)
        .await
        .expect_err("must fail on cycle");
    assert!(format!("{err:#}").contains("cycle"));

    let restored = pm.get_issue(&target).await.unwrap();
    assert_eq!(restored.body, original_body, "body must be restored");
    assert!(
        restored.labels.iter().any(|l| l == &labels::agent("codex")),
        "old agent label must be restored, labels={:?}",
        restored.labels
    );
    assert!(
        !restored
            .labels
            .iter()
            .any(|l| l == &labels::agent("claude-code-acp")),
        "new agent label must be removed during rollback, labels={:?}",
        restored.labels
    );
    assert_eq!(
        restored.blocked_by, original_blocked_by,
        "blocked_by must be restored to prior set"
    );
}

// ─── Test 6 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_abandon_task_with_cascade_marks_descendants_failed() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    // chain: B depends_on A, C depends_on B
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let b_id = br_id(&run_br(dir.path(), &["create", "B", "--silent", "-t", "task"]).unwrap());
    let c = br_id(&run_br(dir.path(), &["create", "C", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["dep", "add", &b_id, &a]).unwrap();
    run_br(dir.path(), &["dep", "add", &c, &b_id]).unwrap();
    let pm = pm_for(&dir).await;

    let bx = batch(
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::AbandonTask {
            issue_id: a.clone(),
            reason: "unrecoverable".into(),
            cascade_descendants: true,
        }],
    );
    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &bx)
        .await
        .expect("abandon apply");

    for id in [&a, &b_id, &c] {
        let issue = pm.get_issue(id).await.unwrap();
        assert_eq!(
            issue.status,
            pm.closed_status(),
            "{id} must be closed (failed) after cascade abandon"
        );
    }
}

// ─── Test 7 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutation_op_abandon_task_without_cascade_does_not_touch_descendants() {
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
        vec![PlanMutationOp::AbandonTask {
            issue_id: a.clone(),
            reason: "lone failure".into(),
            cascade_descendants: false,
        }],
    );
    apply_mutation(pm.clone(), common::server_builder::pro_feature_gate(), &bx)
        .await
        .expect("apply");

    let a_issue = pm.get_issue(&a).await.unwrap();
    assert_eq!(a_issue.status, pm.closed_status(), "A must be closed");

    let b_issue = pm.get_issue(&b_id).await.unwrap();
    assert_ne!(
        b_issue.status,
        pm.closed_status(),
        "B must remain open without cascade"
    );
    let c_issue = pm.get_issue(&c).await.unwrap();
    assert_ne!(
        c_issue.status,
        pm.closed_status(),
        "C must remain open without cascade"
    );
}

// ─── Test 8 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_plan_mutation_applies_batch_atomically() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let pm = pm_for(&dir).await;
    let adv = pm.advanced().unwrap();
    emit_completion_failed(adv, &a).await;
    close_failed(pm.as_ref(), &a).await;

    let result = submit_plan_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::RetryTask {
            issue_id: a.clone(),
        }],
    )
    .await
    .expect("submit ok");

    assert_eq!(result.affected_task_ids, vec![a.clone()]);
    let issue = pm.get_issue(&a).await.unwrap();
    assert_ne!(issue.status, pm.closed_status());
}

// ─── Test 9 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_plan_mutation_validates_no_cycles_post_hoc() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let b = br_id(&run_br(dir.path(), &["create", "B", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["dep", "add", &b, &a]).unwrap();
    let pm = pm_for(&dir).await;

    let err = submit_plan_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::ModifyTaskSpec {
            issue_id: a.clone(),
            new_task: None,
            new_agent: None,
            new_context_files: None,
            new_depends_on: Some(vec![b.clone()]),
        }],
    )
    .await
    .expect_err("must reject cycle");
    assert!(format!("{err:#}").contains("cycle"));
}

// ─── Test 10 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_plan_mutation_rolls_back_on_cycle_detection() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    let b = br_id(&run_br(dir.path(), &["create", "B", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["dep", "add", &b, &a]).unwrap();
    run_br(
        dir.path(),
        &["label", "add", &a, "-l", &labels::agent("codex")],
    )
    .unwrap();
    let pm = pm_for(&dir).await;

    let original = pm.get_issue(&a).await.unwrap();
    let original_body = original.body.clone();

    let _err = submit_plan_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::ModifyTaskSpec {
            issue_id: a.clone(),
            new_task: Some("would-be new body".into()),
            new_agent: Some("claude-code-acp".into()),
            new_context_files: None,
            new_depends_on: Some(vec![b.clone()]),
        }],
    )
    .await
    .expect_err("must fail on cycle");

    let restored = pm.get_issue(&a).await.unwrap();
    assert_eq!(restored.body, original_body, "body must roll back");
    assert!(
        restored.labels.iter().any(|l| l == &labels::agent("codex")),
        "old agent label must be restored"
    );
}

// ─── Test 11 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_plan_mutation_clears_signal_escalated_label_on_success() {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let a = br_id(&run_br(dir.path(), &["create", "A", "--silent", "-t", "task"]).unwrap());
    run_br(dir.path(), &["label", "add", &a, "-l", "signal:escalated"]).unwrap();
    let pm = pm_for(&dir).await;
    let adv = pm.advanced().unwrap();
    emit_completion_failed(adv, &a).await;
    close_failed(pm.as_ref(), &a).await;

    submit_plan_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        Uuid::new_v4(),
        a.clone(),
        vec![PlanMutationOp::RetryTask {
            issue_id: a.clone(),
        }],
    )
    .await
    .expect("retry should succeed");

    let issue = pm.get_issue(&a).await.unwrap();
    assert!(
        !issue.labels.iter().any(|l| l == "signal:escalated"),
        "signal:escalated must be cleared after successful submit; labels={:?}",
        issue.labels
    );
}
