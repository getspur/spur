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
        .expect("delegate_async should return JSON text")["delegation_id"]
        .as_str()
        .expect("delegate_async should return delegation_id")
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn test_shutdown_bounded_with_pending_collectors() -> Result<(), Box<dyn std::error::Error>> {
    const N: usize = 3;

    // Green on main because real delegation tasks are wrapped in
    // `DelegationGuard` when spawned in `spur-core/src/orchestrator.rs`
    // (see the spawn site around line 2728). If a future change stops
    // releasing/sending `respond_to` on task teardown, the same pending
    // collectors exercised here will keep `server.shutdown()` blocked and
    // this regression test will trip the 2s timeout.
    // Phase 1c: detached collectors deliver through the continuation bridge
    // only — the `completed_delegations` map is no longer written. Observe
    // shutdown cleanliness via a counting on_complete callback.
    let failed_count = Arc::new(AtomicUsize::new(0));
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (server, channel) = McpCallbackServer::new(
        &brain_sid,
        None,
        None,
        observing_continuation_ctx(Arc::clone(&failed_count)),
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
                .expect("delegate_async should send a delegation request");
            received_ids.push(request.id.clone());

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
                CallToolRequestParams::new("delegate_async").with_arguments(json_object(json!({
                    "agent": format!("fake-worker-{idx}"),
                    "task": format!("never-complete-{idx}"),
                }))),
            )
            .await?;
        delegation_ids.push(delegation_id(&result));
    }

    let received_ids = ready_rx
        .await
        .expect("holder manager should report when all requests are pending");
    assert_eq!(
        delegation_ids, received_ids,
        "delegate_async responses should match the pending requests observed by the fake workers"
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

    // Phase 1c (source_kind-gated): `AsyncRequested` delegations (the
    // `delegate_async` path) deliver through BOTH the continuation bridge
    // AND `completed_delegations`, so the test now asserts both channels
    // recovered cleanly. `BlockTimeout` (the new auto-reprompt path) is
    // bridge-only and covered by `test_no_double_delivery_on_block_timeout`
    // in spur-core/tests/continuation_integration.rs.
    //
    // (1) Continuation bridge: each dropped `respond_to` sender should have
    //     surfaced as a `Failed { error: "Orchestrator disconnected" }`
    //     completion through the `on_complete` callback.
    assert_eq!(
        failed_count.load(Ordering::SeqCst),
        N,
        "every pending collector should have reported a Failed completion via the \
         continuation bridge after shutdown",
    );

    // (2) `wait_delegation` RPC: legacy callers that drain the map must
    //     still observe the same Failed completion. Regression catcher for
    //     the spec gap noticed in Phase 1c review: if the `AsyncRequested`
    //     arm of the collector ever stops writing the map, this loop
    //     times out instead of returning results, breaking legacy flows.
    for delegation_id in &delegation_ids {
        let result = client
            .call_tool(
                CallToolRequestParams::new("wait_delegation").with_arguments(json_object(json!({
                    "delegation_id": delegation_id,
                }))),
            )
            .await?;
        let payload: Value = serde_json::from_str(&tool_text(&result))
            .expect("wait_delegation should return JSON text");
        assert_eq!(
            payload["status"]["Failed"]["error"],
            Value::String("Orchestrator disconnected".into()),
            "wait_delegation must still surface Failed completions for \
             AsyncRequested collectors (source_kind-gated map write)",
        );
    }

    let _ = client.close().await?;
    let _ = server_service.close().await?;

    let released_ids = holder_manager
        .await
        .expect("holder manager task must not panic");
    assert_eq!(released_ids.len(), N);

    Ok(())
}
