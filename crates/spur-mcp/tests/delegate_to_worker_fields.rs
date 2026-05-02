//! Integration tests for delegate_to_worker field plumbing (bd-1u8p).
//!
//! Verifies that `base` values sent in JSON-RPC arguments survive into
//! the `DelegationRequest` intercepted on the delegation channel.

use rmcp::{
    model::{CallToolRequestParams, JsonObject},
    serve_server, ServiceExt,
};
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::tools::{BaseSpec, BaseTarget, OverlayCommit};
use spur_mcp::{McpCallbackServer, server::DetachedContinuationCtx};
use std::sync::Arc;

mod common;

fn json_object(value: Value) -> JsonObject {
    value
        .as_object()
        .cloned()
        .expect("tool arguments must be a JSON object")
}

fn mock_server() -> (McpCallbackServer, spur_mcp::DelegationChannel) {
    let brain_sid = BrainSessionId::new(SessionId::new());
    McpCallbackServer::new(
        &brain_sid,
        None,
        None,
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        },
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::community_feature_gate(),
    )
}

/// Call delegate_to_worker via the rmcp trait and return the captured
/// DelegationRequest from the delegation channel.
async fn call_delegate_to_worker(
    args: Value,
) -> spur_mcp::tools::DelegationRequest {
    let (server, mut channel) = mock_server();
    let server = Arc::new(server);
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);

    let server_service_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move { serve_server(server, server_io).await }
    });

    let mut client = ().serve(client_io).await.expect("client init");

    let _result = client
        .call_tool(
            CallToolRequestParams::new("delegate_to_worker")
                .with_arguments(json_object(args)),
        )
        .await
        .expect("call_tool should succeed");

    let request = channel
        .request_rx
        .recv()
        .await
        .expect("delegation request should be sent");

    let _ = client.close().await;
    let mut server_service = server_service_task
        .await
        .expect("server bootstrap must not panic")
        .expect("server service ok");
    let _ = server_service.close().await;

    request
}

#[tokio::test]
async fn base_repo_main_survives() {
    let request = call_delegate_to_worker(json!({
        "agent": "claude-code-acp",
        "task": "test task",
        "base": { "kind": "repo_main" }
    }))
    .await;

    assert_eq!(request.base, Some(BaseSpec::RepoMain));
}

#[tokio::test]
async fn base_branch_survives() {
    let request = call_delegate_to_worker(json!({
        "agent": "claude-code-acp",
        "task": "test task",
        "base": { "kind": "branch", "name": "feat/foo" }
    }))
    .await;

    assert_eq!(
        request.base,
        Some(BaseSpec::Branch {
            name: "feat/foo".into()
        })
    );
}

#[tokio::test]
async fn base_commit_survives() {
    let oid = "0000000000000000000000000000000000000000";
    let request = call_delegate_to_worker(json!({
        "agent": "claude-code-acp",
        "task": "test task",
        "base": { "kind": "commit", "oid": oid }
    }))
    .await;

    assert_eq!(
        request.base,
        Some(BaseSpec::Commit { oid: oid.into() })
    );
}

#[tokio::test]
async fn base_with_overlay_survives() {
    let request = call_delegate_to_worker(json!({
        "agent": "claude-code-acp",
        "task": "test task",
        "base": {
            "kind": "with_overlay",
            "base": { "kind": "repo_main" },
            "overlays": [
                {
                    "base_oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "tip_oid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "source_task_id": "test-overlay"
                }
            ]
        }
    }))
    .await;

    match request.base {
        Some(BaseSpec::WithOverlay { base, overlays }) => {
            assert_eq!(base, BaseTarget::RepoMain);
            assert_eq!(overlays.len(), 1);
            assert_eq!(
                overlays[0],
                OverlayCommit {
                    base_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    tip_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    source_task_id: "test-overlay".into(),
                }
            );
        }
        other => panic!("expected WithOverlay, got {:?}", other),
    }
}
