//! T17: JSON-RPC dispatcher tests for `WorkerMcpServer`.
//!
//! Verifies that `tools/list` returns the curated 8-tool subset, that
//! `tools/call` routes by name to the freestanding handlers, that unknown
//! tool names produce `-32601`, and that batched JSON-RPC requests are
//! rejected with `-32600` (per-element token attribution is unsupported).

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use tokio::sync::Mutex;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        panic!("br {args:?} failed (exit {}): {stderr}", out.status);
    }
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
        reconciler_outcomes: Arc::new(Mutex::new(
            spur_mcp::plan::outcomes::OutcomeStore::default(),
        )),
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
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .expect("request");
    resp.json().await.expect("response is JSON")
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn tools_list_returns_8_curated_tools() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let body = call_jsonrpc(&server, &token, "tools/list", serde_json::json!({})).await;
    let tools = body["result"]["tools"]
        .as_array()
        .expect("tools array present");
    assert_eq!(
        tools.len(),
        8,
        "worker subset must expose exactly 8 tools, got: {tools:?}"
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "get_issue",
        "list_issues",
        "get_task_diff",
        "get_plan_status",
        "fetch_outcome_artifact",
        "update_issue",
        "report_signal",
        "report_progress",
    ] {
        assert!(names.contains(&expected), "missing curated tool: {expected}");
    }
    server.shutdown(Duration::from_secs(5)).await;
}

// ─── T23: per-delegation summary event emission ───────────────────────────

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn dispatcher_drop_emits_summary_event_with_correct_counts() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
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

    // One write tool call (update_issue) — also emits an audit
    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "update_issue",
            "arguments": {
                "id": &issue_id,
                "comment": "summary audit"
            }
        }),
    )
    .await;
    assert_eq!(
        body["result"]["ok"].as_bool(),
        Some(true),
        "update_issue should succeed, got: {body}"
    );

    // Complete the delegation
    server.complete_delegation("d-summary", "success");

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
        tool_calls,
        audits_emitted,
        outcome,
        ..
    } = &summaries[0]
    {
        assert_eq!(delegation_id, "d-summary");
        assert_eq!(brain_session_id, "session-summary");
        assert_eq!(*tool_calls, 2, "expected 2 tool calls (get_issue + update_issue)");
        assert_eq!(*audits_emitted, 1, "expected 1 audit (update_issue)");
        assert_eq!(outcome, "success");
    } else {
        panic!("expected WorkerMcpDelegationSummary, got: {:?}", summaries[0]);
    }

    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn report_progress_disabled_returns_success_without_calling_handler() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn report_progress_full_bus_silently_drops_and_returns_success() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn report_progress_enabled_with_capacity_emits_event() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
    let sink = Arc::new(CountingSink { count: AtomicUsize::new(0) });
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn tools_call_get_issue_routes_to_handler() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn tools_call_unknown_tool_returns_method_not_found() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
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
        Some(-32601),
        "unknown brain-only tool must be -32601, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn json_rpc_batched_request_rejected() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
    let (_dir, server) = test_server_with_real_pm().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let url = format!("{}?token={}", server.url(), token);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!([
            {"jsonrpc":"2.0","method":"tools/list","id":1},
            {"jsonrpc":"2.0","method":"tools/list","id":2}
        ]))
        .send()
        .await
        .expect("request");
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(-32600),
        "batches must be -32600 Invalid Request, got: {body}"
    );
    server.shutdown(Duration::from_secs(5)).await;
}
