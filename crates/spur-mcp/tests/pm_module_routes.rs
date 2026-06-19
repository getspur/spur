use std::sync::Arc;

use serde_json::{json, Value};
use spur_pm::mcp::{McpHandlerError, PmMcpDeps, PmMcpModule};
use spur_pm::IssueCreate;
use tempfile::TempDir;

mod common;

#[tokio::test]
async fn brain_route_get_issue_matches_pm_module_response() {
    let dir = TempDir::new().expect("tempdir");
    let (_workspace, pm) = common::beads::init_beads_pm(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "Route issue".to_string(),
            description: Some("route body".to_string()),
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let (server, _channel) = common::server_builder::MockServerBuilder::pro()
        .with_pm_service(Arc::clone(&pm))
        .build();
    let args = json!({ "id": issue_id });

    let direct = PmMcpModule::new(PmMcpDeps {
        pm_service: Some(pm),
        ..Default::default()
    })
    .call("get_issue", args.clone())
    .await
    .expect("direct PM module call succeeds");
    let routed = server.__test_call_tool("get_issue", args).await;

    assert_json_bytes_eq(&routed, &jsonrpc_success(direct));
}

#[tokio::test]
async fn brain_route_graph_subgraph_missing_root_matches_pm_module_error_shape() {
    let (server, _channel) = common::server_builder::MockServerBuilder::pro().build();
    let direct = PmMcpModule::new(PmMcpDeps::default())
        .call("graph_subgraph", json!({}))
        .await
        .expect_err("direct PM module call rejects missing root_id");
    let routed = server.__test_call_tool("graph_subgraph", json!({})).await;

    assert_json_bytes_eq(&routed, &jsonrpc_pm_error(direct));
}

fn jsonrpc_success(result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "result": result
    })
}

fn jsonrpc_pm_error(error: McpHandlerError) -> Value {
    let (code, message) = match error {
        McpHandlerError::InvalidParams(message) => (-32602, message),
        McpHandlerError::NotFound(message) => (-32004, message),
        McpHandlerError::Unauthorized(message) => (-32001, message),
        McpHandlerError::UpstreamPm(message) => {
            (-32603, format!("graph_subgraph failed: {message}"))
        }
        McpHandlerError::Internal(message) => (-32603, message),
    };

    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn assert_json_bytes_eq(actual: &Value, expected: &Value) {
    let actual = serde_json::to_string(actual).expect("serialize actual");
    let expected = serde_json::to_string(expected).expect("serialize expected");
    assert_eq!(actual, expected);
}
