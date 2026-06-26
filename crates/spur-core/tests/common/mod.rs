use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use spur_acp::SpurEventBody;
use spur_core::handlers::{McpHandlerError, WorkerCallContext};
use spur_core::plan::signals::WorkerSignal;
use spur_core::worker_server::WorkerSignalSink;
use spur_core::McpCallbackServer;
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::events::McpEventSink;
use spur_pm::advanced::Comment;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, IssueUpdate, PmService};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::OnceCell;
use uuid::Uuid;

pub mod beads;
pub mod g_strict_harness;
pub mod server_builder;

static LOOPBACK_BINDABLE: OnceCell<bool> = OnceCell::const_new();

pub struct TestWorkerSignalSink {
    funnel: Arc<dyn McpEventSink>,
}

impl TestWorkerSignalSink {
    pub fn new(funnel: Arc<dyn McpEventSink>) -> Self {
        Self { funnel }
    }
}

#[async_trait]
impl WorkerSignalSink for TestWorkerSignalSink {
    async fn report_signal(
        &self,
        _ctx: &WorkerCallContext,
        _args: Value,
    ) -> Result<Value, McpHandlerError> {
        Err(McpHandlerError::Unauthorized(
            "test signal sink is not licensed".into(),
        ))
    }

    async fn report_progress(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        #[derive(serde::Deserialize)]
        struct Args {
            message: String,
            #[serde(default)]
            percent: Option<f64>,
        }

        let Args { message, percent } = serde_json::from_value(args)
            .map_err(|e| McpHandlerError::InvalidParams(format!("invalid args: {e}")))?;
        let _ = self.funnel.try_emit(SpurEventBody::WorkerReportProgress {
            delegation_id: ctx.delegation_id.clone(),
            message,
            percent,
        });

        Ok(json!({ "ok": true }))
    }
}

pub async fn loopback_bindable() -> bool {
    *LOOPBACK_BINDABLE
        .get_or_init(|| async {
            for _ in 0..3 {
                if TcpListener::bind("127.0.0.1:0").await.is_ok() {
                    return true;
                }
            }
            false
        })
        .await
}

#[macro_export]
macro_rules! skip_if_no_loopback {
    ($name:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return;
        }
    };
    ($name:expr, $ret:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return $ret;
        }
    };
}

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
        spur_core::mcp::plan::PlanMcpDeps::from_server(server),
        spur_core::mcp::signals::SignalMcpDeps {
            pm_service: None,
            event_sink: None,
            feature_gate: pro_feature_gate(),
        },
        &spur_acp::config::ContextServiceConfig::default(),
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
