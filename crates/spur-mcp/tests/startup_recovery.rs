use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::params;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::{labels, PlanTask};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer, StartupRecoveryProbe};
use spur_pm::{IssueCreate, IssueUpdate, PmService};
use tempfile::TempDir;

mod common;

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

async fn create_persisted_plan(
    pm: &PmService,
    plan_id: &str,
    owner: Option<&BrainSessionId>,
) -> String {
    let tasks = vec![PlanTask {
        task_id: format!("{plan_id}-task"),
        agent: "codex".into(),
        task: format!("Task for {plan_id}"),
        depends_on: Vec::new(),
        issue_id: None,
        issue_title: None,
        context_files: Vec::new(),
    }];
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm,
        feature_gate.as_ref(),
        plan_id,
        &format!("Epic {plan_id}"),
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");

    if let Some(owner) = owner {
        pm.update_issue(
            &subgraph.epic_id,
            IssueUpdate {
                add_labels: vec![labels::plan_owner(&owner.as_session_id().0)],
                ..Default::default()
            },
        )
        .await
        .expect("stamp owner label");
    }

    spur_mcp::emit_plan_submit_audit(
        pm.advanced().expect("advanced beads backend"),
        plan_id,
        &subgraph,
        spur_mcp::PlanSubmitAuditContext {
            execution_mode: Some("test"),
            brain_session_id: owner.map(BrainSessionId::as_session_id),
            ..Default::default()
        },
    )
    .await;

    subgraph.epic_id
}

async fn create_pending_plan(pm: &PmService, plan_id: &str) -> (String, String) {
    let epic_id = pm
        .create_issue(IssueCreate {
            title: format!("Pending epic {plan_id}"),
            issue_type: Some("epic".to_string()),
            labels: vec![labels::PLAN_PENDING.to_string(), labels::plan_id(plan_id)],
            ..Default::default()
        })
        .await
        .expect("create pending epic");

    let child_id = pm
        .create_issue(IssueCreate {
            title: format!("Pending task {plan_id}"),
            issue_type: Some("task".to_string()),
            parent: Some(epic_id.clone()),
            labels: vec![
                labels::plan_id(plan_id),
                labels::plan_task_id("pending-task"),
            ],
            ..Default::default()
        })
        .await
        .expect("create pending child");

    (epic_id, child_id)
}

fn set_created_at(repo: &Path, issue_id: &str, seconds_ago: i64) {
    let timestamp = (Utc::now() - chrono::Duration::seconds(seconds_ago))
        .to_rfc3339_opts(SecondsFormat::Micros, true);
    let conn = rusqlite::Connection::open(repo.join(".beads/beads.db")).expect("open beads db");
    let changed = conn
        .execute(
            "UPDATE issues SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![timestamp, issue_id],
        )
        .expect("backdate issue");
    assert_eq!(changed, 1, "issue {issue_id} must exist");
}

#[tokio::test]
async fn start_does_not_recover_before_brain_session_is_bound() {
    skip_if_no_loopback!("start_does_not_recover_before_brain_session_is_bound");

    let dir = TempDir::new().expect("tempdir");
    let (_beads, pm) = common::beads::init_beads_pm(dir.path()).await;
    let owner = BrainSessionId::new(SessionId("brain-current".into()));
    create_persisted_plan(pm.as_ref(), "start-plan", Some(&owner)).await;

    let (mut server, _channel) = McpCallbackServer::new(
        None,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let server = Arc::new(server);
    let (_url, handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start should bind without waiting for recovery");

    assert_eq!(
        server.__test_active_plan_count().await,
        0,
        "startup recovery must not hydrate persisted plans before the brain session id is bound"
    );
    assert!(
        server.__test_startup_recovery_pending(),
        "start should record that recovery is pending instead of running it on the bind path"
    );

    server
        .set_brain_session_id(owner)
        .expect("brain_session_id set once");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server.__test_wait_startup_recovery(),
    )
    .await
    .expect("startup recovery should complete after brain session binding");
    assert_eq!(
        server.__test_active_plan_count().await,
        1,
        "pending recovery should run after the brain session id is bound"
    );

    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn start_returns_before_deferred_pending_sweep_finishes() {
    skip_if_no_loopback!("start_returns_before_deferred_pending_sweep_finishes");

    let dir = TempDir::new().expect("tempdir");
    let (_beads, pm) = common::beads::init_beads_pm(dir.path()).await;
    let owner = BrainSessionId::new(SessionId("brain-current".into()));
    let (epic_id, child_id) = create_pending_plan(pm.as_ref(), "pending-startup").await;
    set_created_at(dir.path(), &epic_id, 5);

    let probe = Arc::new(StartupRecoveryProbe::new());
    let _probe_guard = McpCallbackServer::__test_install_startup_recovery_probe(Arc::clone(&probe));

    let (mut server, _channel) = McpCallbackServer::new(
        Some(&owner),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_plan_pending_grace(Duration::from_secs(1));

    let server = Arc::new(server);
    let start = Arc::clone(&server).start();
    tokio::pin!(start);
    let (_url, handle) = tokio::time::timeout(Duration::from_secs(1), &mut start)
        .await
        .expect("start should return while pending sweep is paused")
        .expect("start should bind before pending sweep completes");

    tokio::time::timeout(Duration::from_secs(5), probe.wait_until_entered())
        .await
        .expect("deferred startup task should pause before sweeping pending plans");

    let epic = pm
        .get_issue(&epic_id)
        .await
        .expect("get epic while sweep is paused");
    assert_eq!(
        epic.status, "open",
        "start must return before the deferred sweep closes stale pending epics"
    );
    assert!(
        epic.labels
            .iter()
            .any(|label| label == labels::PLAN_PENDING),
        "start must return before the deferred sweep removes the pending label"
    );

    probe.release();
    tokio::time::timeout(
        Duration::from_secs(5),
        server.__test_wait_startup_recovery(),
    )
    .await
    .expect("deferred startup task should complete after probe release");

    let epic = pm.get_issue(&epic_id).await.expect("get swept epic");
    let child = pm.get_issue(&child_id).await.expect("get swept child");
    assert_eq!(epic.status, pm.closed_status());
    assert_eq!(child.status, pm.closed_status());

    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn dropping_startup_recovery_handle_cancels_in_flight_task() {
    let dir = TempDir::new().expect("tempdir");
    let (_beads, pm) = common::beads::init_beads_pm(dir.path()).await;
    let owner = BrainSessionId::new(SessionId("brain-current".into()));
    create_persisted_plan(pm.as_ref(), "cancel-plan", Some(&owner)).await;

    let probe = Arc::new(StartupRecoveryProbe::new());
    let _probe_guard = McpCallbackServer::__test_install_startup_recovery_probe(Arc::clone(&probe));

    let (mut server, _channel) = McpCallbackServer::new(
        None,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let server = Arc::new(server);
    server.__test_request_startup_recovery();
    assert!(
        server.__test_startup_recovery_pending(),
        "recovery should remain pending until the brain session id is bound"
    );

    server
        .set_brain_session_id(owner)
        .expect("brain_session_id set once");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        probe.wait_until_entered(),
    )
    .await
    .expect("startup recovery should spawn and enter the probed recovery path");

    server.__test_drop_startup_recovery_handle();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        probe.wait_until_dropped(),
    )
    .await
    .expect("dropping the start handle should cancel the in-flight recovery task");
}

#[tokio::test]
async fn recover_persisted_plans_skips_other_owned_and_unowned_plans() {
    let dir = TempDir::new().expect("tempdir");
    let (_beads, pm) = common::beads::init_beads_pm(dir.path()).await;
    let current = BrainSessionId::new(SessionId("brain-current".into()));
    let other = BrainSessionId::new(SessionId("brain-other".into()));

    create_persisted_plan(pm.as_ref(), "owned-plan", Some(&current)).await;
    create_persisted_plan(pm.as_ref(), "other-plan", Some(&other)).await;
    create_persisted_plan(pm.as_ref(), "unowned-plan", None).await;

    let (mut server, _channel) = McpCallbackServer::new(
        Some(&current),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    server
        .__test_recover_persisted_plans()
        .await
        .expect("recover persisted plans");

    assert_eq!(
        server.__test_active_plan_count().await,
        1,
        "recovery should hydrate only plans owned by the current brain session"
    );
}
