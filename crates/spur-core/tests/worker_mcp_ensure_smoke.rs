//! Phase 5 / Task 26 — smoke test for
//! [`Orchestrator::ensure_worker_mcp_server`].
//!
//! Locks the contract added in br-tug: with a fully-configured
//! orchestrator (PmService + FeatureGate) and a per-`BrainSession`
//! `McpCallbackServer`, calling `ensure_worker_mcp_server(brain, mcp)`
//! must boot a real `WorkerMcpServer` (not return the previous
//! "deps not yet wired" stub `Err`) and the returned URL must point
//! at a live `127.0.0.1:<port>` listener.
//!
//! Also exercises the cache contract: a second call with the same
//! `(brain, mcp)` tuple returns the same `Arc` (no duplicate boot).

use std::path::Path;
use std::sync::Arc;

use rmcp::{transport::StreamableHttpClientTransport, ServiceExt};
use spur_acp::config::SpurConfig;
use spur_acp::types::SessionId;
use spur_core::Orchestrator;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_pm::test_workspace::TestBeadsWorkspace;

fn attach_beads_workspace(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

fn embedded_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

#[tokio::test]
async fn ensure_worker_mcp_server_boots_real_listener_and_caches() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    // git init so PmService::try_new accepts the workspace.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git command failed")
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@spur"]);
    git(&["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("README.md"), "test\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "seed"]);

    let beads = TestBeadsWorkspace::init();
    attach_beads_workspace(dir.path(), &beads);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new")
        .expect("expected Some(PmService)");
    let pm = Arc::new(pm);

    let feature_gate = embedded_feature_gate();
    let orch = Orchestrator::new(
        dir.path().into(),
        SpurConfig::default(),
        Some(Arc::clone(&feature_gate)),
    )
    .expect("Orchestrator::new")
    .with_pm_service(Arc::clone(&pm));

    // Build the per-`BrainSession` brain MCP server. The orchestrator's
    // `WorkerMcpFetcher` reads its `PlanResolver` impl + reconciler
    // outcome handle from this instance.
    let brain_session_id: spur_acp::BrainSessionId =
        SessionId("550e8400-e29b-41d4-a716-446655440000".into()).into();
    let ctx = spur_mcp::server::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mcp_server, _channel) = spur_mcp::McpCallbackServer::new(
        Some(&brain_session_id),
        Some(Arc::clone(&pm)),
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    let mcp_server = Arc::new(mcp_server);

    // ── Smoke: ensure_worker_mcp_server returns Ok with a running listener. ─
    let server = orch
        .ensure_worker_mcp_server(&brain_session_id, Arc::clone(&mcp_server))
        .await
        .expect("ensure_worker_mcp_server must boot a real WorkerMcpServer");

    let url = server.url();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "url must point at a real listener, got: {url}"
    );
    assert!(url.contains("/mcp"), "url must end at /mcp, got: {url}");
    assert!(server.is_running(), "freshly booted server must be running");

    // ── Cache: second call returns the same Arc, no duplicate boot. ────────
    let server2 = orch
        .ensure_worker_mcp_server(&brain_session_id, Arc::clone(&mcp_server))
        .await
        .expect("second ensure must succeed");
    assert!(
        Arc::ptr_eq(&server, &server2),
        "second ensure must return the cached Arc (no duplicate boot)"
    );
    assert_eq!(
        server.url(),
        server2.url(),
        "cached server must report the same URL"
    );

    // Token issuance smoke: must produce a non-empty Base64Url token.
    let token = server.issue_token("delegation-smoke", std::time::Duration::from_secs(60));
    assert!(!token.is_empty(), "token must be non-empty");
    assert!(
        !token.contains('\n') && !token.contains(' '),
        "token must be URL-safe Base64Url (no whitespace), got: {token}"
    );

    // WorkerMcpFetcher::fetch_url_token assembles `<url>?token=<token>` for
    // dispatch. Smoke that exact wire shape end-to-end by opening a real RMCP
    // client with the composed URL and listing tools.
    let url_with_token = format!("{}?token={token}", server.url());
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(url_with_token))
        .await
        .expect("rmcp session should open against ensured worker server");
    let tools = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed via ensured worker MCP URL");
    assert!(
        tools.iter().any(|tool| tool.name == "get_issue"),
        "curated worker surface should include get_issue"
    );
    drop(client);
}
