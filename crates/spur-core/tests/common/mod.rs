use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::plan::signals::WorkerSignal;
use spur_mcp::McpCallbackServer;
use spur_pm::advanced::Comment;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, IssueUpdate, PmService};
use tempfile::TempDir;
use uuid::Uuid;

pub fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

pub fn community_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

pub fn install_core_brain_registry(server: &mut McpCallbackServer) {
    let registry = spur_core::mcp::brain_tool_registry(
        spur_core::mcp::delegation::DelegationMcpDeps::from_server(server),
        spur_core::mcp::signals::SignalMcpDeps {
            pm_service: None,
            event_sink: None,
            feature_gate: pro_feature_gate(),
        },
    )
    .expect("core-composed brain registry");
    server.set_tool_registry(registry);
}

pub async fn beads_pm(repo: &Path) -> Arc<PmService> {
    let workspace = TestBeadsWorkspace::init();
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    workspace.copy_db_to(&beads_dir);

    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

pub async fn temp_beads_pm() -> (TempDir, Arc<PmService>) {
    let dir = TempDir::new().expect("tempdir");
    let pm = beads_pm(dir.path()).await;
    (dir, pm)
}

pub async fn create_task(pm: &PmService, title: &str) -> String {
    pm.create_issue(IssueCreate {
        title: title.to_string(),
        issue_type: Some("task".into()),
        ..Default::default()
    })
    .await
    .expect("create_issue must succeed")
}

pub async fn close_task(pm: &PmService, task_id: &str) {
    pm.update_issue(
        task_id,
        IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task");
}

pub async fn comment_texts(pm: &PmService, issue_id: &str) -> Vec<String> {
    pm.advanced()
        .expect("beads advanced backend")
        .list_comments(issue_id)
        .await
        .expect("list comments")
        .into_iter()
        .map(|comment: Comment| comment.body)
        .collect()
}

pub async fn issue_labels(pm: &PmService, issue_id: &str) -> Vec<String> {
    pm.get_issue(issue_id).await.expect("get issue").labels
}

pub fn scope_drift_signal(signal_id: Uuid) -> WorkerSignal {
    WorkerSignal::ScopeDrift {
        signal_id,
        severity: 0.82,
        reason: "auth refactor pulls in 4 new subsystems".into(),
        estimated_subtasks: Some(3),
    }
}
