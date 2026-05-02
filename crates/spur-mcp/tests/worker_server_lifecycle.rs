use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::worker_server::WorkerMcpServer;
use spur_pm::PmService;
use tempfile::TempDir;

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
        panic!("br {args:?} failed (exit {})", out.status);
    }
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

#[tokio::test]
async fn start_binds_listener_and_returns_url() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let feature_gate = test_feature_gate();
    let funnel = test_funnel();

    let server = WorkerMcpServer::start("session-1".into(), pm, feature_gate, funnel)
        .await
        .expect("start must succeed");

    let url = server.url();
    assert!(url.starts_with("http://127.0.0.1:"), "url: {url}");
    assert!(url.contains("/mcp"), "url: {url}");

    // Short timeout so post-shutdown probe fails fast even if the port were
    // somehow still alive.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("client");

    let resp = client
        .get(&url)
        .send()
        .await
        .expect("GET reaches the listener");
    assert!(
        resp.status().is_client_error() || resp.status() == 200,
        "unexpected status: {}",
        resp.status()
    );

    server.shutdown().await;

    // Cancellation must actually close the listener — a follow-up probe
    // should return Err (connection refused / timeout), not a 4xx response.
    let after_shutdown = client.get(&url).send().await;
    assert!(
        after_shutdown.is_err(),
        "listener still reachable after shutdown: {after_shutdown:?}"
    );
}
