use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::plan::PlanTask;

type ResumeCalls = Arc<tokio::sync::Mutex<Vec<crate::server::DispatchOrphanResumeRequest>>>;

fn recording_resume_hook(
    calls: ResumeCalls,
    outcome: crate::server::DispatchOrphanResumeOutcome,
) -> crate::server::DispatchOrphanResumeHook {
    Arc::new(move |request| {
        let calls = Arc::clone(&calls);
        let outcome = outcome.clone();
        Box::pin(async move {
            calls.lock().await.push(request);
            outcome
        })
            as Pin<Box<dyn Future<Output = crate::server::DispatchOrphanResumeOutcome> + Send>>
    })
}

async fn create_dispatch_task(
    dir: &std::path::Path,
    plan_id: &str,
    delegation_id: &str,
) -> (
    Arc<spur_pm::PmService>,
    Arc<spur_license::FeatureGate>,
    String,
) {
    let (_beads, pm) = super::init_beads_pm(dir).await;
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Sync dispatch orphan",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            model: None,
            effort: None,
            config_overrides: None,
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
            add_labels: vec![crate::plan::labels::delegation_id(delegation_id)],
            ..Default::default()
        },
    )
    .await
    .expect("add delegation label");

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
                delegation_id: delegation_id.to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
        ),
    )
    .await
    .expect("add dispatch audit");
    adv.add_comment(
        &task_issue_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::WorkerStarted {
                delegation_id: delegation_id.to_string(),
                worker_branch: "spur/worker/live".to_string(),
                worker_session_id: "worker-session-live".to_string(),
                dispatched_base_oid: "base-oid".to_string(),
            },
        ),
    )
    .await
    .expect("add worker started audit");

    (pm, feature_gate, task_issue_id)
}

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
            model: None,
            effort: None,
            config_overrides: None,
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
async fn resolve_dispatch_orphan_with_resume_keeps_live_dispatch() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
        .await
        .expect("git init");

    let (pm, feature_gate, task_issue_id) =
        create_dispatch_task(dir.path(), "sync-resume-live", "del-live").await;
    let resume_calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let cleared = super::resolve_dispatch_orphan_with_resume(
        Arc::clone(&pm),
        Arc::clone(&feature_gate),
        &task_issue_id,
        Some(recording_resume_hook(
            Arc::clone(&resume_calls),
            crate::server::DispatchOrphanResumeOutcome::Resumed,
        )),
    )
    .await
    .expect("resolve dispatch orphan with resume");

    assert!(!cleared, "live session resume should not clear dispatch");
    let calls = resume_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].issue_id, task_issue_id);
    assert_eq!(calls[0].delegation_id, "del-live");
    assert_eq!(calls[0].worker, "codex");
    assert_eq!(calls[0].worker_session_id, "worker-session-live");
    drop(calls);

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert!(
        issue
            .labels
            .contains(&crate::plan::labels::delegation_id("del-live")),
        "resume must leave dispatch intent intact: {:?}",
        issue.labels
    );
}

#[tokio::test]
async fn resolve_dispatch_orphan_with_resume_falls_back_when_unsupported() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    super::run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
        .await
        .expect("git init");

    let (pm, feature_gate, task_issue_id) =
        create_dispatch_task(dir.path(), "sync-resume-unsupported", "del-unsupported").await;
    let resume_calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let cleared = super::resolve_dispatch_orphan_with_resume(
        Arc::clone(&pm),
        Arc::clone(&feature_gate),
        &task_issue_id,
        Some(recording_resume_hook(
            Arc::clone(&resume_calls),
            crate::server::DispatchOrphanResumeOutcome::Unsupported,
        )),
    )
    .await
    .expect("resolve dispatch orphan with unsupported resume");

    assert!(cleared, "unsupported resume should fall back to clearing");
    assert_eq!(resume_calls.lock().await.len(), 1);
    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert!(
        !issue
            .labels
            .contains(&crate::plan::labels::delegation_id("del-unsupported")),
        "fallback compensation should clear dispatch intent: {:?}",
        issue.labels
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
            model: None,
            effort: None,
            config_overrides: None,
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
