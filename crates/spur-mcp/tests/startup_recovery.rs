use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::{labels, PlanTask};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer, StartupRecoveryProbe};
use spur_pm::{IssueUpdate, PmService};
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
        None,
        None,
        Some("test"),
        owner.map(BrainSessionId::as_session_id),
    )
    .await;

    subgraph.epic_id
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
