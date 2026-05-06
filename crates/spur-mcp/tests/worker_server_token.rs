use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

mod common;
fn br_available() -> bool {
    common::beads::br_available()
}

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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn valid_token_round_trip_header() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let client = reqwest::Client::new();
    let resp = client
        .post(server.url())
        .bearer_auth(&token)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "valid token should pass middleware");
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn valid_token_round_trip_query() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let client = reqwest::Client::new();
    let url = format!("{}?token={}", server.url(), token);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "valid token in query should pass middleware"
    );
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn missing_token_returns_401() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(server.url())
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn malformed_token_returns_401() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(server.url())
        .bearer_auth("totally.not.a.token")
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn tampered_hmac_rejected() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let token = server.issue_token("d-1", Duration::from_secs(60));
    // Mutate the last character of the signature to corrupt the HMAC.
    let mut tampered = token;
    let last = tampered.pop().unwrap();
    let corrupted = if last == 'a' { 'b' } else { 'a' };
    tampered.push(corrupted);

    let client = reqwest::Client::new();
    let resp = client
        .post(server.url())
        .bearer_auth(&tampered)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn expired_token_rejected() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token = server.issue_token_with_expiry("d-1", now - 100);
    let client = reqwest::Client::new();
    let resp = client
        .post(server.url())
        .bearer_auth(&token)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn wrong_brain_session_id_rejected() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
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

    let client = reqwest::Client::new();
    let resp = client
        .post(server_b.url())
        .bearer_auth(&token_from_a)
        .json(&serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":1}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        401,
        "token from server A must be rejected by server B"
    );

    server_a.shutdown(Duration::from_secs(5)).await;
    server_b.shutdown(Duration::from_secs(5)).await;
}

/// Send a header line longer than MAX_HEADER_LINE (8192 bytes) with no
/// trailing newline. The server must detect the truncated read and close the
/// connection with 401 well before the 15-second headers-phase timeout.
#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn long_header_line_without_newline_rejected_quickly() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let (_dir, server) = test_server().await;

    let url = server.url();
    let addr = url.trim_start_matches("http://").trim_end_matches("/mcp");
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");

    // Send a valid request line.
    stream
        .write_all(b"POST /mcp HTTP/1.1\r\n")
        .await
        .expect("write request line");

    // Send a header that exceeds MAX_HEADER_LINE without a newline.
    let long_header = format!("X-Long: {}", "a".repeat(9000));
    stream
        .write_all(long_header.as_bytes())
        .await
        .expect("write long header");

    // The server should reject quickly (well under the 15s header-phase
    // timeout). We give it 10s as a generous upper bound.
    let start = std::time::Instant::now();
    let mut buf = vec![0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
        .await
        .expect("server should respond well under 15s")
        .expect("read");
    let elapsed = start.elapsed();

    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(
        response.contains("401"),
        "expected 401 for truncated header, got: {response}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "should reject quickly, took {:?}",
        elapsed
    );

    server.shutdown(Duration::from_secs(5)).await;
}
