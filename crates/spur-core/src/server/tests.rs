#![allow(clippy::await_holding_lock)]

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

struct ShutdownTaskDropProbe(Arc<AtomicBool>);

impl Drop for ShutdownTaskDropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn test_callback_server() -> Arc<McpCallbackServer> {
    let brain_session_id = BrainSessionId::new(SessionId::new());
    let (server, _channel) = McpCallbackServer::new(
        Some(&brain_session_id),
        None,
        None,
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        },
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::new(FeatureGate::new(
            spur_license::policy::PolicyResolver::embedded(),
        )),
    );
    Arc::new(server)
}

#[tokio::test(flavor = "current_thread")]
async fn root_completion_guard_disarms_after_acknowledged_completion() {
    let server = test_callback_server();
    let root_force = CancellationToken::new();
    *server.root_force_shutdown.lock().unwrap() = Some(root_force.clone());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();

    let abort_on_drop = AbortRootOnDrop::new(Arc::clone(&server));
    let waiter = tokio::spawn(await_root_completion(abort_on_drop, done_rx));
    done_tx
        .send(())
        .expect("completion waiter must remain live");
    waiter.await.expect("completion waiter must not panic");

    assert!(
        !root_force.is_cancelled(),
        "acknowledged graceful completion must not be reclassified as forced"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn root_completion_guard_forces_children_when_waiter_is_aborted() {
    let server = test_callback_server();
    let root_force = CancellationToken::new();
    *server.root_force_shutdown.lock().unwrap() = Some(root_force.clone());
    let (_done_tx, done_rx) = tokio::sync::oneshot::channel();

    let abort_on_drop = AbortRootOnDrop::new(Arc::clone(&server));
    let waiter = tokio::spawn(await_root_completion(abort_on_drop, done_rx));
    waiter.abort();
    let _ = waiter.await;

    assert!(
        root_force.is_cancelled(),
        "aborting the outer waiter must force the callback-server tree"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn graceful_root_completion_is_not_reported_as_forced() {
    let server = test_callback_server();
    let root_force = CancellationToken::new();
    *server.root_force_shutdown.lock().unwrap() = Some(root_force.clone());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    server.__test_set_root_shutdown_tx(shutdown_tx);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let abort_on_drop = AbortRootOnDrop::new(Arc::clone(&server));
    let completion_waiter = tokio::spawn(await_root_completion(abort_on_drop, done_rx));

    server.__test_set_root_handle(tokio::spawn({
        let root_force = root_force.clone();
        async move {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {}
                () = root_force.cancelled() => {}
            }
            done_tx
                .send(())
                .expect("outer completion waiter must remain live");
            // Let the waiter process the acknowledgement before this root join
            // resolves, making graceful/forced classification deterministic.
            tokio::task::yield_now().await;
        }
    }));

    let external_force = CancellationToken::new();
    let forced = server.shutdown_until_forced(&external_force).await;
    completion_waiter
        .await
        .expect("completion waiter must not panic");

    assert!(!forced, "clean root retirement must remain graceful");
    assert!(
        !root_force.is_cancelled(),
        "normal completion must disarm the outer force guard"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_until_forced_joins_handles_taken_by_graceful_path() {
    let server = test_callback_server();
    let task_dropped = Arc::new(AtomicBool::new(false));
    let task_started = Arc::new(Notify::new());
    let recovery_task = AbortOnDropHandle::new(tokio::spawn({
        let task_dropped = Arc::clone(&task_dropped);
        let task_started = Arc::clone(&task_started);
        async move {
            let _probe = ShutdownTaskDropProbe(task_dropped);
            task_started.notify_one();
            std::future::pending::<()>().await;
        }
    }));
    task_started.notified().await;
    server.startup_recovery.lock().unwrap().handle = Some(StartupRecoveryTaskHandle {
        cancel_tx: None,
        handle: recovery_task,
    });

    let force = CancellationToken::new();
    let shutdown = tokio::spawn({
        let server = Arc::clone(&server);
        let force = force.clone();
        async move { server.shutdown_until_forced(&force).await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.startup_recovery.lock().unwrap().handle.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("graceful shutdown never took the recovery handle");

    force.cancel();
    let forced = tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .expect("forced root shutdown must acknowledge promptly")
        .expect("shutdown task panicked");

    assert!(forced);
    assert!(
        task_dropped.load(Ordering::SeqCst),
        "root shutdown returned before its taken recovery task acknowledged abort"
    );
}

#[test]
fn telemetry_tool_name_maps_known_tools_and_hashes_unknown() {
    use spur_telemetry::events::IntoProp;

    assert_eq!(
        McpCallbackServer::telemetry_mcp_tool_name("submit_plan").into_prop(),
        json!("submit_plan")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_tool_name("dispatch_task").into_prop(),
        json!("dispatch_task")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_tool_name("review_task").into_prop(),
        json!("review_task")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_tool_name("get_task_diff").into_prop(),
        json!("get_task_diff")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_tool_name("list_tools").into_prop(),
        json!("list_tools")
    );

    let unknown = "customer_email@example.com";
    let hashed = McpCallbackServer::telemetry_mcp_tool_name(unknown).into_prop();
    assert_eq!(
        hashed,
        spur_telemetry::tier2_events::HashedShort::from_sha256_prefix(unknown).into_prop()
    );
    assert_ne!(hashed, json!(unknown));
}

#[test]
fn telemetry_server_name_maps_known_servers_and_hashes_unknown() {
    use spur_telemetry::events::IntoProp;

    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("github").into_prop(),
        json!("github")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("posthog").into_prop(),
        json!("posthog")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("spur-mcp").into_prop(),
        json!("spur-mcp")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("stitch").into_prop(),
        json!("stitch")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("playwright").into_prop(),
        json!("playwright")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("context7").into_prop(),
        json!("context7")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("firebase").into_prop(),
        json!("firebase")
    );
    assert_eq!(
        McpCallbackServer::telemetry_mcp_server_name("sequential-thinking").into_prop(),
        json!("sequential-thinking")
    );

    let unknown = "private-server-name";
    let hashed = McpCallbackServer::telemetry_mcp_server_name(unknown).into_prop();
    assert_eq!(
        hashed,
        spur_telemetry::tier2_events::HashedShort::from_sha256_prefix(unknown).into_prop()
    );
    assert_ne!(hashed, json!(unknown));
}

#[test]
fn telemetry_outcome_from_json_rpc_response_tracks_errors() {
    assert_eq!(
        McpCallbackServer::telemetry_outcome_from_json_rpc_response(&JsonRpcResponse::success(
            Value::Null,
            json!({})
        )),
        spur_telemetry::tier1_events::Outcome::Ok
    );
    assert_eq!(
        McpCallbackServer::telemetry_outcome_from_json_rpc_response(
            &JsonRpcResponse::internal_error(Value::Null, "failed")
        ),
        spur_telemetry::tier1_events::Outcome::Error
    );
}

static BEADS_SQLITE_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn beads_sqlite_serial_guard() -> MutexGuard<'static, ()> {
    BEADS_SQLITE_SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
}

#[cfg(test)]
fn attach_beads_workspace(repo: &std::path::Path, w: &spur_pm::test_workspace::TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    // Copy db + WAL + SHM (beads_rust uses WAL mode and skips checkpoint on
    // Drop; bare `fs::copy(beads.db)` loses every uncheckpointed write).
    w.copy_db_to(&beads_dir);
}

#[cfg(test)]
async fn init_beads_pm(
    repo: &std::path::Path,
) -> (
    spur_pm::test_workspace::TestBeadsWorkspace,
    std::sync::Arc<spur_pm::PmService>,
) {
    let w = spur_pm::test_workspace::TestBeadsWorkspace::init();
    attach_beads_workspace(repo, &w);

    let pm = std::sync::Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    (w, pm)
}

mod build_worker_info_tests;
mod clobber_review_tests;
mod merge_plan_tests;
mod recover_orphaned_dispatch_tests;
mod sync_tests;
mod versioned_cache_tests;
