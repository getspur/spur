#![allow(clippy::await_holding_lock)]

use super::*;
use std::sync::{Mutex, MutexGuard, OnceLock};

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
