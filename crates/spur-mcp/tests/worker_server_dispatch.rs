//! T17: JSON-RPC dispatcher tests for `WorkerMcpServer`.
//!
//! Verifies that `tools/list` returns the curated worker-tool subset, that
//! `tools/call` routes by name to the freestanding handlers, that unknown
//! tool names produce `-32601`, and that batched JSON-RPC requests are
//! rejected at the transport decoder (per-element token attribution is
//! unsupported).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::{
    model::CallToolRequestParams,
    service::ServiceError,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::Value;
use spur_acp::SpurEventBody;
use spur_graph::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use tokio::sync::Mutex;

mod common;

struct CwdGuard {
    original: std::path::PathBuf,
}

impl CwdGuard {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

async fn pm_service_fixture(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

fn test_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

struct NullSink;

impl McpEventSink for NullSink {
    fn emit(&self, _event: SpurEventBody) {}
}

/// Sink that panics on any emission — used to verify the disabled-gate
/// short-circuits before the handler is invoked.
struct PanicSink;

impl McpEventSink for PanicSink {
    fn emit(&self, _event: SpurEventBody) {
        panic!("emit must not be called when progress is disabled");
    }
    fn try_emit(&self, _event: SpurEventBody) -> Result<(), SpurEventBody> {
        panic!("try_emit must not be called when progress is disabled");
    }
}

/// Sink that simulates a full broadcast bus — `try_emit` always returns
/// `Err` so the handler must silently drop and still return success.
struct FullSink;

impl McpEventSink for FullSink {
    fn emit(&self, _event: SpurEventBody) {
        panic!("emit must not be called — try_emit should be used");
    }
    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        Err(event)
    }
}

/// Counting sink that records each event it receives. Used to verify the
/// happy path — when progress is enabled AND the bus has capacity, the
/// handler IS called and the event IS emitted.
struct CountingSink {
    count: AtomicUsize,
}

impl McpEventSink for CountingSink {
    fn emit(&self, _event: SpurEventBody) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        self.emit(event);
        Ok(())
    }
}

/// Sink that captures every event body it receives via `try_emit`.
struct RecordingSink {
    events: std::sync::Mutex<Vec<SpurEventBody>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl McpEventSink for RecordingSink {
    fn emit(&self, event: SpurEventBody) {
        self.events.lock().unwrap().push(event);
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        self.emit(event);
        Ok(())
    }
}

struct NullPlanResolver;

#[async_trait]
impl PlanResolver for NullPlanResolver {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<Mutex<PlanState>>, String> {
        Err(format!("test resolver: unknown plan_id '{plan_id}'"))
    }
}

fn test_deps(pm: Arc<PmService>) -> WorkerMcpDeps {
    test_deps_with_funnel(pm, Arc::new(NullSink))
}

fn test_deps_with_funnel(pm: Arc<PmService>, funnel: Arc<dyn McpEventSink>) -> WorkerMcpDeps {
    WorkerMcpDeps {
        pm_service: pm,
        feature_gate: test_feature_gate(),
        funnel,
        plan_resolver: Arc::new(NullPlanResolver),
        reconciler_outcomes: Arc::new(
            Mutex::new(spur_mcp::plan::outcomes::OutcomeStore::default()),
        ),
        outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        repo_root: None,
    }
}

async fn test_server_with_real_pm() -> (TempDir, Arc<WorkerMcpServer>) {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let server = WorkerMcpServer::start("session-disp".into(), test_deps(pm))
        .await
        .expect("start must succeed");
    (dir, server)
}

async fn test_server_with_issue() -> (TempDir, Arc<WorkerMcpServer>, String) {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "dispatch happy path".into(),
            description: Some("issue body".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");
    let server = WorkerMcpServer::start("session-disp".into(), test_deps(pm))
        .await
        .expect("start must succeed");
    (dir, server, issue_id)
}

async fn call_jsonrpc(
    server: &Arc<WorkerMcpServer>,
    token: &str,
    method: &str,
    params: Value,
) -> Value {
    let config =
        StreamableHttpClientTransportConfig::with_uri(server.url()).auth_header(token.to_string());
    let client =
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .expect("rmcp client initialize");

    let response = match method {
        "tools/list" => match client.list_all_tools().await {
            Ok(tools) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "tools": tools }
            }),
            Err(error) => service_error_response(error),
        },
        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .expect("params.name is required")
                .to_string();
            let mut request = CallToolRequestParams::new(tool_name);
            request.arguments = params.get("arguments").and_then(|v| v.as_object()).cloned();
            match client.call_tool(request).await {
                Ok(result) => {
                    let payload = result
                        .structured_content
                        .clone()
                        .unwrap_or_else(|| serde_json::to_value(result).expect("serialize result"));
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": payload
                    })
                }
                Err(error) => service_error_response(error),
            }
        }
        other => panic!("unsupported JSON-RPC method in test helper: {other}"),
    };

    drop(client);
    response
}

fn service_error_response(error: ServiceError) -> Value {
    let (code, message) = match &error {
        ServiceError::McpError(mcp_error) => {
            (i64::from(mcp_error.code.0), mcp_error.message.to_string())
        }
        _ => (-32603, error.to_string()),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

#[tokio::test]
async fn tools_list_returns_curated_worker_tools_including_code_graph_reads() {
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/list", serde_json::json!({})).await;
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools array present");
    let expected = [
        "get_issue",
        "list_issues",
        "get_task_diff",
        "get_plan_status",
        "fetch_outcome_artifact",
        "code_search",
        "code_resolve",
        "code_file_symbols",
        "code_symbol_info",
        "code_read_symbol",
        "code_callers",
        "code_callees",
        "code_subgraph",
        "code_symbol_history",
        "report_signal",
        "report_progress",
    ];
    assert_eq!(
        tools.len(),
        expected.len(),
        "worker subset must expose exactly {} tools, got: {tools:?}",
        expected.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in expected {
        assert!(
            names.contains(&expected),
            "missing curated tool: {expected}"
        );
    }
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn tools_call_code_graph_metadata_tools_are_reachable() {
    let (dir, server) = test_server_with_real_pm().await;
    std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
    std::fs::write(
        dir.path().join(".spur/graph-index.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "header": {
                "graph_index_version": GRAPH_INDEX_VERSION_TEMPORAL
            },
            "manifest_version": "worker-test",
            "graph_content_hash": "worker-hash",
            "files": [
                { "stable_file_id": "file-src-lib", "file_path": "src/lib.rs" }
            ],
            "symbols": [
                {
                    "stable_symbol_id": "symbol-launch",
                    "file_path": "src/lib.rs",
                    "byte_range": [0, 8],
                    "line_range": [1, 3],
                    "entity_name": "launch_order",
                    "qualified_name": "launch_order",
                    "symbol_kind": "function",
                    "anchor_hash": "hash-symbol-launch",
                    "enclosing_scope": null
                }
            ],
            "edges": [],
            "tombstones": []
        }))
        .expect("encode graph fixture"),
    )
    .expect("write graph fixture");
    let _cwd = CwdGuard::enter(dir.path());
    let token = server.issue_token("d-1", Duration::from_secs(60));

    for (tool, arguments, expected_key) in [
        (
            "code_resolve",
            serde_json::json!({ "selector": "launch_order" }),
            "candidates",
        ),
        (
            "code_file_symbols",
            serde_json::json!({ "file": "src/lib.rs" }),
            "symbols",
        ),
        (
            "code_symbol_info",
            serde_json::json!({ "selector": "launch_order" }),
            "symbol",
        ),
    ] {
        let body = call_jsonrpc(
            &server,
            &token,
            "tools/call",
            serde_json::json!({"name": tool, "arguments": arguments}),
        )
        .await;
        assert!(
            body["result"].get(expected_key).is_some(),
            "{tool} should return result.{expected_key}, got: {body}"
        );
    }

    server.shutdown(Duration::from_secs(5)).await;
}

// ─── T23: per-delegation summary event emission ───────────────────────────

#[tokio::test]
async fn dispatcher_drop_emits_summary_event_with_correct_counts() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let sink = Arc::new(RecordingSink::new());
    let server = WorkerMcpServer::start(
        "session-summary".into(),
        test_deps_with_funnel(Arc::clone(&pm), Arc::clone(&sink) as Arc<dyn McpEventSink>),
    )
    .await
    .expect("start must succeed");

    server.register_delegation(
        "d-summary".into(),
        spur_mcp::worker_server::DelegationContext {
            enable_worker_progress: false,
        },
    );
    let token = server.issue_token("d-summary", Duration::from_secs(60));

    // Create an issue so both read and write calls succeed.
    let issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "summary test".into(),
            description: Some("body".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    // One read tool call (get_issue)
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "get_issue", "arguments": {"id": &issue_id}}),
    )
    .await;
    assert!(
        body.get("result").is_some(),
        "get_issue should succeed, got: {body}"
    );

    // A second tool call so summary event reports two distinct tools.
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "list_issues",
            "arguments": {}
        }),
    )
    .await;
    assert!(
        body.get("result").is_some(),
        "list_issues should succeed, got: {body}"
    );

    // Complete the delegation
    server.complete_delegation("d-summary", "success");

    {
        let events = sink.events.lock().unwrap();
        let summaries: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SpurEventBody::WorkerMcpDelegationSummary { .. }))
            .collect();
        assert_eq!(
            summaries.len(),
            1,
            "expected exactly one summary event, got: {events:?}"
        );

        if let SpurEventBody::WorkerMcpDelegationSummary {
            delegation_id,
            brain_session_id,
            calls_total,
            calls_by_tool,
            errors,
            ..
        } = &summaries[0]
        {
            assert_eq!(delegation_id, "d-summary");
            assert_eq!(brain_session_id, "session-summary");
            assert_eq!(
                *calls_total, 2,
                "expected 2 tool calls (get_issue + list_issues)"
            );
            assert_eq!(calls_by_tool.get("get_issue"), Some(&1));
            assert_eq!(calls_by_tool.get("list_issues"), Some(&1));
            assert_eq!(*errors, 0, "no calls returned errors");
        } else {
            panic!(
                "expected WorkerMcpDelegationSummary, got: {:?}",
                summaries[0]
            );
        }
    }

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn report_progress_disabled_returns_success_without_calling_handler() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let server = WorkerMcpServer::start(
        "session-disp".into(),
        test_deps_with_funnel(pm, Arc::new(PanicSink)),
    )
    .await
    .expect("start must succeed");
    server.register_delegation(
        "d-1".into(),
        spur_mcp::worker_server::DelegationContext {
            enable_worker_progress: false,
        },
    );
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "hi"}}),
    )
    .await;
    assert_eq!(
        body["result"]["ok"].as_bool(),
        Some(true),
        "should return success when progress is disabled, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn report_progress_full_bus_silently_drops_and_returns_success() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let server = WorkerMcpServer::start(
        "session-disp".into(),
        test_deps_with_funnel(pm, Arc::new(FullSink)),
    )
    .await
    .expect("start must succeed");
    server.register_delegation(
        "d-1".into(),
        spur_mcp::worker_server::DelegationContext {
            enable_worker_progress: true,
        },
    );
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "hi"}}),
    )
    .await;
    assert_eq!(
        body["result"]["ok"].as_bool(),
        Some(true),
        "should return success even when bus is full, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn report_progress_enabled_with_capacity_emits_event() {
    let sink = Arc::new(CountingSink {
        count: AtomicUsize::new(0),
    });
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let server = WorkerMcpServer::start(
        "session-disp".into(),
        test_deps_with_funnel(pm, sink.clone()),
    )
    .await
    .expect("start must succeed");
    server.register_delegation(
        "d-1".into(),
        spur_mcp::worker_server::DelegationContext {
            enable_worker_progress: true,
        },
    );
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "hi"}}),
    )
    .await;
    assert_eq!(body["result"]["ok"].as_bool(), Some(true));
    assert_eq!(
        sink.count.load(Ordering::SeqCst),
        1,
        "event must be emitted exactly once"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn tools_call_get_issue_routes_to_handler() {
    let (_dir, server, issue_id) = test_server_with_issue().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "get_issue", "arguments": {"id": issue_id}}),
    )
    .await;
    assert_eq!(
        body["result"]["id"].as_str(),
        Some(issue_id.as_str()),
        "dispatcher should return raw issue JSON, got: {body}"
    );
    assert_eq!(body["result"]["body"].as_str(), Some("issue body"));
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_method_not_found() {
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "delegate_to_worker", "arguments": {}}),
    )
    .await;
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(-32602),
        "unknown worker tool should be invalid params/tool-not-found under native RMCP routing, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn json_rpc_batched_request_rejected() {
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let client = reqwest::Client::new();
    let initialize = client
        .post(server.url())
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "spur-worker-dispatch-test",
                    "version": "0.0.0"
                }
            }
        }))
        .send()
        .await
        .expect("initialize request");
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("initialize response must carry mcp-session-id")
        .to_string();

    // rmcp's typed client API cannot encode batched JSON-RPC payload arrays,
    // so this negative wire-contract assertion must send raw JSON over HTTP.
    let response = client
        .post(server.url())
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header("mcp-session-id", session_id)
        .bearer_auth(token)
        .json(&serde_json::json!([
            {"jsonrpc":"2.0","method":"tools/list","id":1},
            {"jsonrpc":"2.0","method":"tools/list","id":2}
        ]))
        .send()
        .await
        .expect("request");
    let status = response.status();
    let body = response.text().await.expect("response body");
    assert_eq!(
        status,
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "batches must be rejected as malformed transport requests, got {status}: {body}"
    );
    assert!(
        body.contains("JsonRpcMessage"),
        "batch rejection should identify malformed JSON-RPC payload, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn curated_tool_calls_route_and_deserialize_params() {
    let (_dir, server, issue_id) = test_server_with_issue().await;
    server.register_delegation(
        "d-1".into(),
        spur_mcp::worker_server::DelegationContext {
            enable_worker_progress: true,
        },
    );
    let token = server.issue_token("d-1", Duration::from_secs(60));

    let list_issues = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "list_issues", "arguments": {"limit": 10}}),
    )
    .await;
    assert!(
        list_issues.get("error").is_none() || list_issues["error"].is_null(),
        "list_issues should deserialize + route, got: {list_issues}"
    );
    assert!(
        list_issues["result"].as_array().is_some_and(|issues| issues
            .iter()
            .any(|issue| issue["id"].as_str() == Some(&issue_id))),
        "list_issues should include seeded issue {issue_id}, got: {list_issues}"
    );

    let get_plan_status = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "get_plan_status", "arguments": {"plan_id": "missing-plan"}}),
    )
    .await;
    assert_eq!(
        get_plan_status["error"]["code"].as_i64(),
        Some(-32602),
        "unknown plan_id should hit get_plan_status handler and return invalid params"
    );

    let get_task_diff = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "get_task_diff",
            "arguments": {"plan_id": "missing-plan", "task_id": &issue_id}
        }),
    )
    .await;
    assert_eq!(
        get_task_diff["error"]["code"].as_i64(),
        Some(-32602),
        "unknown plan_id should hit get_task_diff handler and return invalid params"
    );

    let fetch_outcome = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "fetch_outcome_artifact", "arguments": {"delegation_id": "d-1"}}),
    )
    .await;
    assert!(
        matches!(
            fetch_outcome["error"]["code"].as_i64(),
            Some(-32004) | Some(-32001)
        ),
        "fetch_outcome_artifact should route and return domain error, got: {fetch_outcome}"
    );

    let report_signal = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "report_signal",
            "arguments": {
                "task_id": &issue_id,
                "signal": {
                    "kind": "scope_drift",
                    "signal_id": "550e8400-e29b-41d4-a716-446655440999",
                    "severity": 0.5,
                    "reason": "dispatch-routing-test",
                    "estimated_subtasks": 2
                }
            }
        }),
    )
    .await;
    let signal_error = report_signal["error"]["code"].as_i64();
    let signal_recorded = report_signal["result"]["recorded"].as_bool();
    assert!(
        signal_error == Some(-32001) || signal_recorded == Some(true),
        "report_signal should route + deserialize (either gated or recorded), got: {report_signal}"
    );

    let report_progress = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({"name": "report_progress", "arguments": {"message": "working"}}),
    )
    .await;
    assert_eq!(
        report_progress["result"]["ok"].as_bool(),
        Some(true),
        "report_progress should deserialize + route, got: {report_progress}"
    );

    server.shutdown(Duration::from_secs(5)).await;
}
