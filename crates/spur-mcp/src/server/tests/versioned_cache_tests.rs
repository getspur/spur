use crate::plan::audit_sentinel::{
    encode_comment, AuditSentinelKind, CompletionState, SENTINEL_PREFIX,
};
use crate::plan::PlanTask;

use super::{run_git_capture, BeadsVersion};

async fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    run_git_capture(dir.path(), None, &["init", "-q"])
        .await
        .expect("git init");
    run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
        .await
        .expect("git config user.email");
    run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
        .await
        .expect("git config user.name");
    dir
}

fn sample_tasks() -> Vec<PlanTask> {
    vec![PlanTask {
        task_id: "task-1".into(),
        agent: "codex".into(),
        task: "Do thing".into(),
        depends_on: Vec::new(),
        issue_id: None,
        issue_title: None,
        context_files: Vec::new(),
    }]
}

async fn derive_version(
    pm: &spur_pm::PmService,
    feature_gate: &spur_license::FeatureGate,
    epic_id: &str,
) -> BeadsVersion {
    super::McpCallbackServer::derive_beads_version(pm, feature_gate, epic_id)
        .await
        .expect("derive beads version")
}

#[tokio::test]
async fn derive_beads_version_advances_through_bd334_task_audit_sequence() {
    let dir = init_repo().await;
    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();

    let sg = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "plan-1",
        "Epic",
        None,
        &sample_tasks(),
    )
    .await
    .expect("build epic subgraph");

    let adv = pm.advanced().expect("advanced backend");
    crate::emit_plan_submit_audit(adv, "plan-1", &sg, crate::PlanSubmitAuditContext::default())
        .await;

    let task_issue_id = sg.task_map.get("task-1").expect("task issue id");
    let mut seen = std::collections::HashSet::new();
    seen.insert(derive_version(pm.as_ref(), feature_gate.as_ref(), &sg.epic_id).await);

    let audits = vec![
        AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/B".into()),
            result_summary: Some("completion 1".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::ReviewFeedback {
            delegation_id: "del-1".into(),
            attempt: 1,
            feedback: "request changes".into(),
            worker_branch: Some("spur/worker/B".into()),
            summary: Some("needs edits".into()),
            reuse_prior_worktree: Some(true),
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/B".into()),
            result_summary: Some("completion 2".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::Approval {
            delegation_id: "del-1".into(),
        },
    ];

    for audit in audits {
        adv.add_comment(task_issue_id, &encode_comment(&audit))
            .await
            .expect("task audit write");
        let version = derive_version(pm.as_ref(), feature_gate.as_ref(), &sg.epic_id).await;
        assert!(
            seen.insert(version),
            "token collision: task-level audit write must produce a new cache token"
        );
    }
}

#[tokio::test]
async fn derive_beads_version_does_not_collide_across_plan_restart_same_plan_id() {
    let dir = init_repo().await;
    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let adv = pm.advanced().expect("advanced backend");

    let sg1 = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "plan-1",
        "Epic",
        None,
        &sample_tasks(),
    )
    .await
    .expect("build first epic subgraph");
    crate::emit_plan_submit_audit(
        adv,
        "plan-1",
        &sg1,
        crate::PlanSubmitAuditContext::default(),
    )
    .await;
    let task1 = sg1.task_map.get("task-1").expect("task1 issue id");
    adv.add_comment(
        task1,
        &encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-a1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/A".into()),
            result_summary: Some("attempt 1".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .expect("task1 completion");
    let before_restart = derive_version(pm.as_ref(), feature_gate.as_ref(), &sg1.epic_id).await;

    let sg2 = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "plan-1",
        "Epic amended",
        Some(&sg1.epic_id),
        &sample_tasks(),
    )
    .await
    .expect("reuse existing epic for restart");
    crate::emit_plan_submit_audit(
        adv,
        "plan-1",
        &sg2,
        crate::PlanSubmitAuditContext::default(),
    )
    .await;
    let task2 = sg2.task_map.get("task-1").expect("task2 issue id");
    adv.add_comment(
        task2,
        &encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-a2".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/B".into()),
            result_summary: Some("attempt 2".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .expect("task2 completion");

    let after_restart = derive_version(pm.as_ref(), feature_gate.as_ref(), &sg1.epic_id).await;
    assert_ne!(
        before_restart, after_restart,
        "restart history for same plan_id must not collide with prior cache token"
    );
}

#[tokio::test]
async fn derive_beads_version_includes_malformed_task_audit_sentinel() {
    let dir = init_repo().await;
    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();

    let sg = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "plan-1",
        "Epic",
        None,
        &sample_tasks(),
    )
    .await
    .expect("build epic subgraph");

    let adv = pm.advanced().expect("advanced backend");
    crate::emit_plan_submit_audit(adv, "plan-1", &sg, crate::PlanSubmitAuditContext::default())
        .await;
    let before = derive_version(pm.as_ref(), feature_gate.as_ref(), &sg.epic_id).await;

    let task_issue_id = sg.task_map.get("task-1").expect("task issue id");
    adv.add_comment(task_issue_id, &format!("{SENTINEL_PREFIX}\n{{"))
        .await
        .expect("malformed sentinel write");

    let after = derive_version(pm.as_ref(), feature_gate.as_ref(), &sg.epic_id).await;
    assert_ne!(
        before, after,
        "malformed sentinel is intentionally included in hash token"
    );
}
