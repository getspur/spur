use std::sync::Arc;

use rmcp::{model::CallToolRequestParams, transport::StreamableHttpClientTransport, ServiceExt};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer, WorkerInfo};

mod common;

fn test_continuation_ctx() -> DetachedContinuationCtx {
    // No-op on_complete: test harnesses don't route continuations.
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

#[tokio::test]
async fn rmcp_client_can_initialize_list_tools_and_call_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    skip_if_no_loopback!(
        "rmcp_client_can_initialize_list_tools_and_call_tool",
        Ok(())
    );
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![WorkerInfo {
        name: "worker-a".into(),
        tier: Some("generalist".into()),
        description: Some("RMCP smoke worker".into()),
        good_for: vec!["transport smoke tests".into()],
        avoid_for: Vec::new(),
        output_shape: Some("text".into()),
        cost_tier: Some("low".into()),
    }]);

    let server = Arc::new(server);
    let (url, handle) = server.clone().start().await?;

    let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await?;
    let tools = client.list_all_tools().await?;
    assert!(tools.iter().any(|tool| tool.name == "delegate_to_worker"));
    assert!(tools
        .iter()
        .any(|tool| tool.name == "list_available_workers"));

    let result = client
        .call_tool(CallToolRequestParams::new("list_available_workers"))
        .await?;
    let serialized = serde_json::to_string(&result)?;
    assert!(
        serialized.contains("worker-a"),
        "expected list_available_workers result to include the configured worker: {serialized}",
    );

    drop(client);
    handle.abort();
    let _ = handle.await;
    server.shutdown().await;
    Ok(())
}
