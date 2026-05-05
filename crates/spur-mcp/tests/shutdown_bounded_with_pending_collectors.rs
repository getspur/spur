use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    model::{CallToolRequestParams, JsonObject},
    serve_server, ServiceExt,
};
use serde_json::{json, Value};
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use tokio::sync::{oneshot, Notify};

mod common;

/// Observing continuation ctx: counts callback invocations and, for each
/// `Failed` completion, tracks the error string so tests can assert that the
/// collector finished cleanly after `shutdown()`.
fn observing_continuation_ctx(failed_count: Arc<AtomicUsize>) -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker| {
            let failed_count = Arc::clone(&failed_count);
            Box::pin(async move {
                if matches!(cont.payload.status, DelegationStatus::Failed { .. }) {
                    failed_count.fetch_add(1, Ordering::SeqCst);
                }
            })
        }),
    }
}

fn json_object(value: Value) -> JsonObject {
    value
        .as_object()
        .cloned()
        .expect("tool arguments must be a JSON object")
}

fn tool_text(result: &rmcp::model::CallToolResult) -> String {
    let value = serde_json::to_value(result).expect("tool result should serialize");
    value["content"][0]["text"]
        .as_str()
        .expect("tool result should contain a text content item")
        .to_string()
}

fn delegation_id(result: &rmcp::model::CallToolResult) -> String {
    serde_json::from_str::<Value>(&tool_text(result))
        .expect("delegate_to_worker should return JSON text")["delegation_id"]
        .as_str()
        .expect("delegate_to_worker should return delegation_id")
        .to_string()
}

/// INV-ASYNC-4 — shutdown boundedness. Post-Phase-4 (deprecated tool removal),
/// `delegate_async` / `wait_delegation` are gone. The async-first
/// `delegate_to_worker` handler with the default `inline_wait=0` hands every
/// delegation straight to a detached `BlockTimeout` collector, which is
/// exactly what we need to exercise for this invariant: N pending collectors
/// holding oneshot receivers while `shutdown()` runs.
///
/// Pending respond_to senders are dropped during shutdown; each collector
/// surfaces the drop as `DelegationStatus::Failed { error: "Orchestrator
/// disconnected" }` through the continuation bridge. Because the
/// `BlockTimeout` source skips the `completed_delegations` map write
/// (INV-ASYNC-2), the bridge is the sole delivery channel and we assert on
/// its callback counter.
#[tokio::test(flavor = "current_thread")]
async fn test_shutdown_bounded_with_pending_collectors() -> Result<(), Box<dyn std::error::Error>> {
    const N: usize = 3;

    let failed_count = Arc::new(AtomicUsize::new(0));
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (server, channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        observing_continuation_ctx(Arc::clone(&failed_count)),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    let server = Arc::new(server);
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);
    let server_service_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move { serve_server(server, server_io).await }
    });

    let release = Arc::new(Notify::new());
    let release_for_manager = Arc::clone(&release);
    let (ready_tx, ready_rx) = oneshot::channel();

    let holder_manager = tokio::spawn(async move {
        let mut request_rx = channel.request_rx;
        let mut holder_tasks = Vec::with_capacity(N);
        let mut received_ids = Vec::with_capacity(N);

        for _ in 0..N {
            let request = request_rx
                .recv()
                .await
                .expect("delegate_to_worker should send a delegation request");
            received_ids.push(request.id.to_string());

            let release = Arc::clone(&release_for_manager);
            holder_tasks.push(tokio::spawn(async move {
                release.notified().await;
                drop(request.respond_to);
            }));
        }

        let _ = ready_tx.send(received_ids.clone());

        for holder in holder_tasks {
            holder.await.expect("holder task must not panic");
        }

        received_ids
    });

    let mut client = ().serve(client_io).await?;
    let mut server_service = server_service_task
        .await
        .expect("server bootstrap task must not panic")?;

    let mut delegation_ids = Vec::with_capacity(N);
    for idx in 0..N {
        let result = client
            .call_tool(
                CallToolRequestParams::new("delegate_to_worker").with_arguments(json_object(
                    json!({
                        "agent": format!("fake-worker-{idx}"),
                        "task": format!("never-complete-{idx}"),
                    }),
                )),
            )
            .await?;
        delegation_ids.push(delegation_id(&result));
    }

    let received_ids = ready_rx
        .await
        .expect("holder manager should report when all requests are pending");
    assert_eq!(
        delegation_ids, received_ids,
        "delegate_to_worker responses should match the pending requests observed by the fake workers"
    );

    tokio::task::yield_now().await;

    let shutdown = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server.shutdown().await;
        }
    });

    tokio::task::yield_now().await;
    release.notify_waiters();

    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown must stay bounded with pending collectors")?;

    // BlockTimeout path: each dropped `respond_to` sender should have
    // surfaced as a `Failed { error: "Orchestrator disconnected" }`
    // completion through the `on_complete` callback (the continuation
    // bridge is the sole delivery channel — INV-ASYNC-2 skips the map
    // write for BlockTimeout source).
    assert_eq!(
        failed_count.load(Ordering::SeqCst),
        N,
        "every pending collector should have reported a Failed completion via the \
         continuation bridge after shutdown",
    );

    let _ = client.close().await?;
    let _ = server_service.close().await?;

    let released_ids = holder_manager
        .await
        .expect("holder manager task must not panic");
    assert_eq!(released_ids.len(), N);

    Ok(())
}
