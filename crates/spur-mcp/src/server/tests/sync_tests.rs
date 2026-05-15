use std::sync::Arc;

use crate::plan::PlanTask;

#[tokio::test]
async fn resolve_dispatch_orphan_proceeds_when_ready_label_is_label_only() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
        .await
        .expect("git init");

    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "sync-ready-label-only",
        "Sync ready label only",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Recover orphan".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");

    pm.update_issue(
        &task_issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![
                crate::plan::labels::delegation_id("del-a"),
                crate::plan::labels::READY_FOR_REVIEW.to_string(),
            ],
            ..Default::default()
        },
    )
    .await
    .expect("add delegation and ready label");

    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = pm.advanced().expect("advanced beads backend");
    adv.add_comment(
        &task_issue_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                delegation_id: "del-a".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
        ),
    )
    .await
    .expect("add dispatch audit");

    let cleared =
        super::resolve_dispatch_orphan(Arc::clone(&pm), Arc::clone(&feature_gate), &task_issue_id)
            .await
            .expect("resolve dispatch orphan");
    assert!(
        cleared,
        "label-only ready-for-review should not veto recovery"
    );
}

#[tokio::test]
async fn resolve_dispatch_orphan_skips_when_delegation_label_has_no_audit() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
        .await
        .expect("git init");

    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "sync-label-only-delegation",
        "Sync label only delegation",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Recover orphan".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");

    pm.update_issue(
        &task_issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::delegation_id("del-label-only")],
            ..Default::default()
        },
    )
    .await
    .expect("add label-only delegation");

    let cleared =
        super::resolve_dispatch_orphan(Arc::clone(&pm), Arc::clone(&feature_gate), &task_issue_id)
            .await
            .expect("resolve dispatch orphan");
    assert!(
        !cleared,
        "label-only delegation must not recover without audit"
    );

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == &crate::plan::labels::delegation_id("del-label-only")),
        "delegation label should remain when no audit attestation exists"
    );
}
