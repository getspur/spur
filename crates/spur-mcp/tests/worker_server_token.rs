use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::{
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::Mutex;

mod common;

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

async fn test_pm_service_empty(repo: &Path) -> Arc<PmService> {
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

fn test_funnel() -> Arc<dyn McpEventSink> {
    Arc::new(NullSink)
}

struct NullPlanResolver;

#[async_trait]
impl PlanResolver for NullPlanResolver {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<Mutex<PlanState>>, String> {
        Err(format!("test resolver: unknown plan_id '{plan_id}'"))
    }
}

fn test_deps(pm: Arc<PmService>) -> WorkerMcpDeps {
    WorkerMcpDeps {
        pm_service: pm,
        feature_gate: test_feature_gate(),
        funnel: test_funnel(),
        plan_resolver: Arc::new(NullPlanResolver),
        reconciler_outcomes: Arc::new(
            Mutex::new(spur_mcp::plan::outcomes::OutcomeStore::default()),
        ),
        outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        repo_root: None,
    }
}

async fn test_server() -> (TempDir, Arc<WorkerMcpServer>) {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;
    let server = WorkerMcpServer::start("session-1".into(), test_deps(pm))
        .await
        .expect("start must succeed");
    (dir, server)
}

async fn list_tool_names_query(url: &str, token: &str) -> Result<Vec<String>, String> {
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{url}?token={token}"
        )))
        .await
        .map_err(|error| error.to_string())?;
    let tools = client
        .list_all_tools()
        .await
        .map_err(|error| error.to_string())?;
    let names = tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    drop(client);
    Ok(names)
}

async fn list_tool_names_no_auth(url: &str) -> Result<Vec<String>, String> {
    let client =
        ().serve(StreamableHttpClientTransport::from_uri(url.to_string()))
            .await
            .map_err(|error| error.to_string())?;
    let tools = client
        .list_all_tools()
        .await
        .map_err(|error| error.to_string())?;
    let names = tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    drop(client);
    Ok(names)
}

async fn list_tool_names_header(url: &str, token: &str) -> Result<Vec<String>, String> {
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token);
    let client =
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())?;
    let tools = client
        .list_all_tools()
        .await
        .map_err(|error| error.to_string())?;
    let names = tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    drop(client);
    Ok(names)
}

fn assert_auth_denied(result: Result<Vec<String>, String>) {
    let error = result.expect_err("expected auth to be denied");
    let lower = error.to_ascii_lowercase();
    assert!(
        lower.contains("-32001") || lower.contains("401"),
        "expected HTTP 401 or MCP -32001 auth denial, got: {error}"
    );
}

fn assert_header_phase_rejected(result: Result<Vec<String>, String>) {
    let error = result.expect_err("expected oversized header to be rejected");
    let lower = error.to_ascii_lowercase();
    assert!(
        lower.contains("400")
            || lower.contains("401")
            || lower.contains("431")
            || lower.contains("header"),
        "expected auth-denied error, got: {error}"
    );
}

#[tokio::test]
async fn valid_token_round_trip_header() {
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let tools = list_tool_names_header(&server.url(), &token)
        .await
        .expect("valid bearer token should open rmcp session");
    assert!(
        !tools.is_empty(),
        "expected tools/list to succeed via bearer header"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn valid_token_round_trip_query() {
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let mut via_query = list_tool_names_query(&server.url(), &token)
        .await
        .expect("valid query token should open rmcp session");
    let mut via_header = list_tool_names_header(&server.url(), &token)
        .await
        .expect("valid bearer token should open rmcp session");
    via_query.sort();
    via_header.sort();
    assert_eq!(
        via_query, via_header,
        "token in query and Authorization header must expose identical tool surface"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn missing_token_returns_401() {
    let (_dir, server) = test_server().await;
    assert_auth_denied(list_tool_names_no_auth(&server.url()).await);
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn malformed_token_returns_401() {
    let (_dir, server) = test_server().await;
    assert_auth_denied(list_tool_names_header(&server.url(), "totally.not.a.token").await);
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn tampered_hmac_rejected() {
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    // Mutate the last character of the signature to corrupt the HMAC.
    let mut tampered = token;
    let last = tampered.pop().unwrap();
    let corrupted = if last == 'a' { 'b' } else { 'a' };
    tampered.push(corrupted);

    assert_auth_denied(list_tool_names_header(&server.url(), &tampered).await);
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn expired_token_rejected() {
    let (_dir, server) = test_server().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token = server.issue_token_with_expiry("d-1", now - 100);
    assert_auth_denied(list_tool_names_header(&server.url(), &token).await);
    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn wrong_brain_session_id_rejected() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;

    let server_a = WorkerMcpServer::start("session-a".into(), test_deps(pm.clone()))
        .await
        .expect("start A");
    let server_b = WorkerMcpServer::start("session-b".into(), test_deps(pm))
        .await
        .expect("start B");

    let token_from_a = server_a.issue_token("d-1", Duration::from_secs(60));
    assert_auth_denied(list_tool_names_header(&server_b.url(), &token_from_a).await);

    server_a.shutdown(Duration::from_secs(5)).await;
    server_b.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn long_bearer_token_rejected_quickly() {
    let (_dir, server) = test_server().await;
    let long_token = "a".repeat(9000);

    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        list_tool_names_header(&server.url(), &long_token),
    )
    .await
    .expect("oversized header token should fail quickly");
    let elapsed = start.elapsed();

    assert_header_phase_rejected(result);
    assert!(
        elapsed < Duration::from_secs(10),
        "should reject quickly, took {:?}",
        elapsed
    );

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn session_reuse_survives_expired_token_after_session_open() {
    let (_dir, server) = test_server().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix time")
        .as_secs();
    // Token expires very soon. Session-open succeeds, then token expires.
    let short_lived = server.issue_token_with_expiry("d-1", now + 1);

    let url_with_token = format!("{}?token={}", server.url(), short_lived);
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(url_with_token))
        .await
        .expect("session-open should succeed with short-lived token");
    let first = client
        .list_all_tools()
        .await
        .expect("first call should succeed before token expiry");
    assert!(!first.is_empty(), "tools/list should return curated tools");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let second = client
        .list_all_tools()
        .await
        .expect("session-bound request should succeed after token expiry");
    assert!(
        !second.is_empty(),
        "session-bound follow-up should not require revalidating expired token"
    );
    drop(client);

    server.shutdown(Duration::from_secs(5)).await;
}
