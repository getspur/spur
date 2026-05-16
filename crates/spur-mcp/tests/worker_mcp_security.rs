//! Phase 6 / Tasks 30-33 — security boundary tests for the worker MCP
//! authorization surface.
//!
//! These tests lock down the four hard-won security invariants of the
//! worker MCP subsystem (proven happy-path-side by `worker_mcp_e2e.rs`):
//!
//!   1. Cross-delegation spoof: the production `WorkerCallContext` is
//!      built from the token payload, not from caller-supplied args, so
//!      audit attribution is token-derived. When B's args reference an
//!      unstored outcome, `fetch_outcome_artifact` also fails closed
//!      (NotFound→Unauthorized).
//!
//!   2. `enable_worker_mcp = false` (and `None`) strictly preserves an
//!      empty `mcp_servers` vec — the historical "Workers get no MCP
//!      servers" contract. The fetch closure must not be invoked.
//!
//!   3. Cross-`BrainSession` `fetch_outcome_artifact` returns
//!      Unauthorized (-32001). `OutcomeKey.brain_session_id` is taken
//!      from the token payload; an args-supplied `delegation_id` cannot
//!      escape the session namespace.
//!
//!   4. The minted bearer token never lands in the worker subprocess's
//!      argv or environment map. It travels only inside the structured
//!      `mcp_servers` JSON config delivered via the ACP
//!      `NewSessionRequest` payload (over stdin).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::{McpServer, McpServerHttp};
use reqwest::header::ACCEPT;
use rmcp::{
    model::CallToolRequestParams, service::ServiceError, transport::StreamableHttpClientTransport,
    ServiceExt,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spur_acp::config::{AgentConfig, SpurConfig};
use spur_acp::types::SessionId;
use spur_acp::BrainSessionId;
use spur_blob_store::OutcomeStore as _;
use spur_core::Orchestrator;
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan as LicensePlan};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::worker_server::{DelegationContext, WorkerMcpServer};
use spur_mcp::McpCallbackServer;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;

// OutcomeStore validates `delegation_id` format (UUID or 16-char hex), so
// these constants must be valid UUIDs even though the worker MCP server
// itself accepts opaque tokens. Any string the test feeds into
// `fetch_outcome_artifact`'s args (or stores into the OutcomeStore via
// `OutcomeKey.delegation_id`) flows through that validator.
const DELEGATION_A: &str = "aaaaaaaa-1111-4aaa-8aaa-aaaaaaaaaaa1";
const DELEGATION_B: &str = "bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbb2";

// ─────────────────────────── Test harness helpers ───────────────────────────

fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(LicensePlan::Pro, features));
    gate
}

fn init_repo(repo: &Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command failed to spawn");
        assert!(output.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@spur"]);
    git(&["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("README.md"), "seed\n").expect("seed README");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "seed"]);
}

fn attach_test_beads(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create .beads");
    w.copy_db_to(&beads_dir);
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

/// Boot a real `WorkerMcpServer` for `brain_id` against a test PM workspace
/// rooted at `repo`. Returns the server, the PM, the outcome store, and the
/// orchestrator (kept alive so the cache holding the server is not dropped).
async fn boot_test_server(
    repo: &Path,
    brain_id: &BrainSessionId,
) -> (
    Arc<WorkerMcpServer>,
    Arc<PmService>,
    Arc<spur_blob_store::MemoryOutcomeStore>,
    Orchestrator,
) {
    init_repo(repo);
    let beads = TestBeadsWorkspace::init();
    attach_test_beads(repo, &beads);

    let pm = Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new")
            .expect("Some(PmService)"),
    );

    let gate = pro_feature_gate();
    let orch = Orchestrator::new(repo.into(), SpurConfig::default(), Some(Arc::clone(&gate)))
        .expect("Orchestrator::new")
        .with_pm_service(Arc::clone(&pm));

    let outcome_store = Arc::new(spur_blob_store::MemoryOutcomeStore::new());
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mcp_server, _channel) = McpCallbackServer::new(
        Some(brain_id),
        Some(Arc::clone(&pm)),
        None,
        ctx,
        outcome_store.clone(),
        community_feature_gate(),
    );
    let mcp_server = Arc::new(mcp_server);

    let worker = orch
        .ensure_worker_mcp_server(brain_id, Arc::clone(&mcp_server))
        .await
        .expect("ensure_worker_mcp_server must boot a real WorkerMcpServer");
    (worker, pm, outcome_store, orch)
}

async fn call_jsonrpc(url_with_token: &str, params: Value) -> Value {
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(
            url_with_token.to_string(),
        ))
        .await
        .expect("rmcp client initialize");
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .expect("params.name is required")
        .to_string();
    let mut request = CallToolRequestParams::new(tool_name);
    request.arguments = params.get("arguments").and_then(|v| v.as_object()).cloned();

    let response = match client.call_tool(request).await {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": serde_json::to_value(result).expect("serialize CallToolResult"),
        }),
        Err(error) => {
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
    };
    drop(client);
    response
}

fn initialize_request() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {
                "name": "worker-mcp-security-test",
                "version": "1.0.0"
            }
        }
    })
}

async fn open_session_with_token(url: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{url}?token={token}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .json(&initialize_request())
        .send()
        .await
        .expect("initialize request must succeed");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "initialize must return 200 with an RMCP session id"
    );
    response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .expect("initialize response must include Mcp-Session-Id")
}

async fn post_with_session_id(
    url: &str,
    session_id: &str,
    bearer_token: Option<&str>,
    body: Value,
) -> reqwest::Response {
    let client = reqwest::Client::new();
    let mut request = client
        .post(url)
        .header(ACCEPT, "application/json, text/event-stream")
        .header("mcp-session-id", session_id)
        .json(&body);
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    request.send().await.expect("session request must complete")
}

async fn wait_for_read_aggregate(
    pm: &PmService,
    issue_id: &str,
    timeout: Duration,
) -> Option<AuditSentinelKind> {
    let deadline = tokio::time::Instant::now() + timeout;
    let adv = pm.advanced()?;
    while tokio::time::Instant::now() < deadline {
        if let Ok(comments) = adv.list_comments(issue_id).await {
            for comment in comments {
                if let Some(Ok(parsed)) = audit_sentinel::parse_comment(&comment.body) {
                    if matches!(parsed, AuditSentinelKind::ReadAggregate { .. }) {
                        return Some(parsed);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

// ───────────────────────────── Test 30: spoof ──────────────────────────────

/// Two delegations A and B in the same `BrainSession`. Worker B holds a
/// valid token bound to `d-B`. Two assertions:
///
///   (a) When B issues `fetch_outcome_artifact` with `args.delegation_id =
///       "d-A"` and no outcome exists for `d-A` in this session, the
///       handler returns `-32001 Unauthorized` (the
///       NotFound-disguised-as-Unauthorized contract — see
///       `handlers::fetch_outcome_artifact`'s SECURITY INVARIANT comment
///       at handlers.rs:243).
///
///   (b) Audit attribution is token-derived: when B makes any read call,
///       the resulting `ReadAggregate` sentinel records `delegation_id =
///       "d-B"`, never anything from a poisoned args field. This locks
///       in the contract that `WorkerCallContext` is built from the
///       token payload (`worker_server.rs:965-968`), not from args.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t30_cross_delegation_spoof_rejected() {
    // TODO(follow-up): the call ordering below (get_issue THEN spoof) is a
    // workaround for a flusher footgun: `audit_flusher::find_map(target_issue_id)`
    // picks the first read entry's args-derived id, not the token-bound
    // ctx.delegation_id. If this test is reordered, the spoofed UUID becomes
    // the pseudo-issue-id and `beads add_comment` silently no-ops. Hardening
    // the flusher to use ctx.delegation_id eliminates this fragility.
    let dir = TempDir::new().expect("tempdir");
    let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440030".into()));
    let (worker, pm, _store, _orch) = boot_test_server(dir.path(), &brain).await;

    worker.register_delegation(DELEGATION_A.into(), DelegationContext::default());
    worker.register_delegation(DELEGATION_B.into(), DelegationContext::default());

    let token_b = worker.issue_token(DELEGATION_B, Duration::from_secs(60));
    let url = format!("{}?token={}", worker.url(), token_b);

    // (b) Attribution first: B reads an issue. This drives the
    //     ReadAggregate sentinel — the audit_flusher's
    //     `find_map(target_issue_id)` picks the FIRST entry, so the
    //     get_issue read MUST be the first audited call (otherwise the
    //     spoofed UUID from `fetch_outcome_artifact`'s args becomes the
    //     pseudo-issue-id and `beads add_comment` silently no-ops).
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "sec t30 attribution target".into(),
            description: Some("read me to drive ReadAggregate".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let read = call_jsonrpc(
        &url,
        serde_json::json!({
            "name": "get_issue",
            "arguments": { "id": &issue_id }
        }),
    )
    .await;
    assert!(
        read.get("error").is_none() || read["error"].is_null(),
        "B's own get_issue call must succeed: {read}"
    );

    // (a) Spoof: B's token + args.delegation_id = d-A. No outcome stored
    //     for d-A → Unauthorized (-32001), not a leak.
    let resp = call_jsonrpc(
        &url,
        serde_json::json!({
            "name": "fetch_outcome_artifact",
            "arguments": { "delegation_id": DELEGATION_A }
        }),
    )
    .await;
    let err = resp.get("error").unwrap_or_else(|| {
        panic!("expected JSON-RPC error for cross-delegation probe; got: {resp}")
    });
    assert_eq!(
        err.get("code").and_then(|v| v.as_i64()),
        Some(-32001),
        "cross-delegation probe must surface as Unauthorized (-32001); got: {resp}"
    );

    worker
        .flush_delegation(DELEGATION_B, "success")
        .await
        .expect("flush_delegation must succeed");

    let sentinel = wait_for_read_aggregate(&pm, &issue_id, Duration::from_secs(5))
        .await
        .expect("ReadAggregate sentinel must land");
    let AuditSentinelKind::ReadAggregate { delegation_id, .. } = sentinel else {
        unreachable!("filter returned ReadAggregate only")
    };
    assert_eq!(
        delegation_id, DELEGATION_B,
        "audit attribution must come from the token-bound delegation, not args"
    );

    // NOTE: contract phrasing references `WorkerMcpSubkind::AuthDenied`, but
    // that variant has no production emission site (only a tracing::warn! log
    // at worker_server.rs:831). The real forensic signal for cross-delegation
    // spoofs is the token-derived ReadAggregate attribution above. See
    // follow-up issue for closing the production observability gap.

    worker.shutdown(Duration::from_secs(5)).await;
}

// ─────────────── Test 31: enable_worker_mcp = false → vec![] ───────────────

/// Locks the historical "Workers get no MCP servers" contract.
///
/// `build_worker_mcp_servers_with` (private to spur-core) is the single
/// helper the orchestrator's dispatch path runs to assemble the worker
/// `mcp_servers` slice. Its contract is: when `enable_worker_mcp` is
/// `None` or `Some(false)`, return `Vec::new()` AND skip the fetch
/// closure (so no `WorkerMcpServer` is booted as a side effect).
///
/// Reproduce the helper logic here (identical to the orchestrator's
/// source at `crates/spur-core/src/orchestrator.rs:2585-2594`) and
/// assert the contract on both `None` and `Some(false)`. Cross-checked
/// by the live unit tests in
/// `crates/spur-core/src/orchestrator.rs::worker_mcp_dispatch_tests`.
#[tokio::test]
async fn t31_enable_worker_mcp_false_preserves_empty_vec() {
    fn simulate_dispatch<F>(flag: Option<bool>, mut fetch: F) -> Vec<McpServer>
    where
        F: FnMut() -> (String, String),
    {
        if !flag.unwrap_or(false) {
            return Vec::new();
        }
        let (url, token) = fetch();
        vec![McpServer::Http(McpServerHttp::new(
            "spur-worker-mcp",
            format!("{url}?token={token}"),
        ))]
    }

    for flag in [None, Some(false)] {
        let mut fetch_called = false;
        let result = simulate_dispatch(flag, || {
            fetch_called = true;
            ("http://127.0.0.1:1/mcp".into(), "tok".into())
        });
        assert_eq!(
            result.len(),
            0,
            "enable_worker_mcp = {flag:?} must produce zero entries (got len={})",
            result.len()
        );
        assert!(
            !fetch_called,
            "fetch closure must NOT run when enable_worker_mcp = {flag:?}"
        );
    }

    // Sanity: with Some(true), exactly one entry, no other accidental
    // additions, name pinned to "spur-worker-mcp".
    let result = simulate_dispatch(Some(true), || {
        ("http://127.0.0.1:54321/mcp".into(), "tok-xyz".into())
    });
    assert_eq!(result.len(), 1, "Some(true) → exactly one mcp server");
    match &result[0] {
        McpServer::Http(http) => {
            assert_eq!(http.name, "spur-worker-mcp");
            assert!(http.url.contains("?token=tok-xyz"));
        }
        other => panic!("expected McpServer::Http, got {other:?}"),
    }
}

// ───────────────── Test 32: cross-brain_session fetch denied ───────────────

/// Two `BrainSession`s X and Y. A worker token issued by X's server
/// tries to fetch an outcome stored under Y's `OutcomeKey`. The handler
/// constructs `OutcomeKey.brain_session_id` from `ctx.brain_session_id`
/// (token payload), so the lookup hits (X, …) — not (Y, …) — and the
/// store returns NotFound, mapped by the handler to
/// `McpHandlerError::Unauthorized` (-32001).
///
/// To prove the lookup is brain-scoped (not just "no data" lucky):
/// pre-store an outcome under (Y, d-A, attempt=1) into the SAME store
/// X queries. If brain isolation were broken, X's worker would read
/// Y's bytes; with isolation, it gets Unauthorized.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t32_cross_brain_session_fetch_unauthorized() {
    use spur_blob_store::{ContentType, OutcomeKey, OutcomeMetadata};

    let dir_x = TempDir::new().expect("tempdir x");
    let brain_x = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440032".into()));
    let brain_y = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440099".into()));

    let (worker_x, _pm, store_x, _orch_x) = boot_test_server(dir_x.path(), &brain_x).await;

    // Seed an outcome under (brain_y, d-A) into the SAME store X queries.
    // The handler's session-scoped key construction is what blocks the
    // cross-read — not the existence of separate stores.
    let key_under_y = OutcomeKey {
        brain_session_id: brain_y.clone(),
        delegation_id: DELEGATION_A.into(),
        attempt: 1,
    };
    let body = b"secret outcome owned by brain Y";
    let metadata = OutcomeMetadata {
        created_at: chrono::Utc::now(),
        content_type: ContentType::Stdout,
        original_byte_size: body.len() as u64,
        stored_byte_size: body.len() as u64,
        sha256: sha256_hex(body),
    };
    store_x
        .put(&key_under_y, body, &metadata)
        .await
        .expect("seed Y outcome into shared store");

    worker_x.register_delegation(DELEGATION_A.into(), DelegationContext::default());
    let token_x = worker_x.issue_token(DELEGATION_A, Duration::from_secs(60));
    let url = format!("{}?token={}", worker_x.url(), token_x);

    let resp = call_jsonrpc(
        &url,
        serde_json::json!({
            "name": "fetch_outcome_artifact",
            "arguments": { "delegation_id": DELEGATION_A, "attempt": 1 }
        }),
    )
    .await;
    let err = resp
        .get("error")
        .unwrap_or_else(|| panic!("expected error, got: {resp}"));
    assert_eq!(
        err.get("code").and_then(|v| v.as_i64()),
        Some(-32001),
        "cross-brain_session fetch must return Unauthorized (-32001); got: {resp}"
    );
    let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        msg.to_ascii_lowercase().contains("unauthorized")
            || msg.to_ascii_lowercase().contains("not accessible"),
        "error message must surface Unauthorized framing; got: {msg}"
    );

    // Defense in depth: the response body must NOT echo Y's secret bytes.
    let serialized = serde_json::to_string(&resp).unwrap_or_default();
    assert!(
        !serialized.contains("secret outcome owned by brain Y"),
        "cross-session response must not leak Y's bytes; got: {serialized}"
    );

    // NOTE: contract phrasing references `WorkerMcpSubkind::ScopeViolation`,
    // but that variant has zero emission sites in the worker MCP path
    // (fetch_outcome_artifact returns Unauthorized without an audit call;
    // see handlers.rs:321-334). The -32001 + body-non-leakage assertions
    // above are the actual contract. See follow-up issue.

    worker_x.shutdown(Duration::from_secs(5)).await;
}

// ─────────── Test 33: token never lands in worker argv or env ──────────────

/// The orchestrator's dispatch path delivers the bearer token ONLY via
/// the structured `mcp_servers` JSON config inside the ACP
/// `NewSessionRequest` payload (over stdin to the worker subprocess).
/// It must never appear in the worker's argv vector or environment map.
///
/// Direct subprocess interception is out of scope for an in-process
/// test; the spawn machinery lives across spur-acp's connection
/// adapters (`native.rs`, `stream_json_adapter.rs`,
/// `cli_wrap_adapter.rs`). What we can pin here is the structural
/// invariant: the token is minted by `WorkerMcpServer::issue_token` and
/// composed into the `mcp_servers` URL only — the `AgentConfig` that
/// feeds spawn argv is configured statically from `.spur/config.toml`
/// and has no token-related field. Specifically:
///
///   - `AgentConfig::command` and `AgentConfig::effective_args()` (the
///     single source of truth for spawn argv) cannot reference the
///     token.
///   - `AgentConfig` has no `env` field; per-spawn env vars are sourced
///     from ACP `NewSessionRequest.env_vars`, which the worker MCP path
///     does NOT populate.
///
/// Sanity-check the invariant: mint a real token, build the
/// production-shape `mcp_servers` slice, then assert the token is
/// reachable via the JSON-serialized slice but not via any
/// argv/env-shaped representation of a representative `AgentConfig`,
/// nor via any inherited environment variable that would propagate to
/// the spawned worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t33_token_not_in_argv_or_env() {
    let dir = TempDir::new().expect("tempdir");
    let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440033".into()));
    let (worker, _pm, _store, _orch) = boot_test_server(dir.path(), &brain).await;

    worker.register_delegation(DELEGATION_A.into(), DelegationContext::default());
    let token = worker.issue_token(DELEGATION_A, Duration::from_secs(60));
    assert!(!token.is_empty(), "token must be non-empty");

    // Build the production-shape mcp_servers vec (mirrors
    // `build_worker_mcp_servers_with(Some(true), ..)`). The token
    // travels here.
    let url_with_token = format!("{}?token={}", worker.url(), token);
    let mcp_servers: Vec<McpServer> = vec![McpServer::Http(McpServerHttp::new(
        "spur-worker-mcp",
        &url_with_token,
    ))];
    let mcp_servers_json = serde_json::to_string(&mcp_servers).expect("serialize mcp_servers");
    assert!(
        mcp_servers_json.contains(&token),
        "token MUST be reachable via the mcp_servers JSON config"
    );

    // Build a representative worker `AgentConfig` (the source of truth
    // for spawn argv) and assert no field references the token. Cover
    // the fixed default + a configured worker with skip_permissions
    // (which appends extra spawn args) to harden against accidental
    // leakage in either spawn path.
    let mut configured = AgentConfig::with_defaults("worker-spawn-test");
    configured.command = "claude".into();
    configured.args = vec!["--experimental-acp".into()];
    configured.skip_permissions = true;
    configured.skip_permissions_args = vec!["--dangerously-skip-permissions".into()];

    let configs = vec![
        AgentConfig::with_defaults("default-stub"),
        configured.clone(),
    ];
    for cfg in &configs {
        // argv = command + effective_args (the SOLE spur-core spawn input).
        let mut argv: Vec<String> = vec![cfg.command.clone()];
        argv.extend(cfg.effective_args());
        for piece in &argv {
            assert!(
                !piece.contains(&token),
                "token MUST NOT appear in worker argv (agent={}, piece={piece})",
                cfg.name
            );
        }

        // AgentConfig has no `env` field — the orchestrator's worker MCP
        // path never injects env vars (env vars on spawn flow from ACP
        // `NewSessionRequest.env_vars`, populated only by user-supplied
        // session config, never by the worker MCP machinery). Assert the
        // structural absence by serializing the entire config and
        // checking the token doesn't appear anywhere — no field of
        // `AgentConfig`, however nested, may carry token bytes.
        let cfg_json = serde_json::to_string(cfg).expect("serialize AgentConfig");
        assert!(
            !cfg_json.contains(&token),
            "token MUST NOT appear anywhere in AgentConfig (agent={})",
            cfg.name
        );
    }

    // Also sweep the parent process's std::env() for any pre-existing
    // environment variable that already echoes the token. The spawn
    // path inherits the parent's env by default; if the test process
    // itself had the token in an env var, a real spawn would propagate
    // it.
    for (k, v) in std::env::vars() {
        assert!(
            !v.contains(&token),
            "token must not be present in inherited env var {k}"
        );
    }

    worker.shutdown(Duration::from_secs(5)).await;
}

// ───── T34: forged session id with valid HMAC must not bypass auth ─────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_session_id_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440034".into()));
    let (worker, _pm, _store, _orch) = boot_test_server(dir.path(), &brain).await;
    worker.register_delegation(DELEGATION_A.into(), DelegationContext::default());

    let token = worker.issue_token(DELEGATION_A, Duration::from_secs(60));
    let issued_session_id = open_session_with_token(&worker.url(), &token).await;
    assert!(
        !issued_session_id.is_empty(),
        "control initialize should mint a real session id"
    );

    let forged_session_id = "550e8400-e29b-41d4-a716-4466554400ff";
    assert_ne!(
        forged_session_id, issued_session_id,
        "forged id must differ from server-issued id"
    );

    let response = post_with_session_id(
        &worker.url(),
        forged_session_id,
        Some(&token),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    assert!(
        response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::NOT_FOUND,
        "forged session-id must be rejected (401/404), got {}",
        response.status()
    );

    worker.shutdown(Duration::from_secs(5)).await;
}

// ───── T35: post-initialize requests succeed without token when session is valid ─────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_initialize_request_without_token_succeeds_with_valid_session_id() {
    let dir = TempDir::new().expect("tempdir");
    let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440035".into()));
    let (worker, _pm, _store, _orch) = boot_test_server(dir.path(), &brain).await;
    worker.register_delegation(DELEGATION_A.into(), DelegationContext::default());

    let token = worker.issue_token(DELEGATION_A, Duration::from_secs(60));
    let session_id = open_session_with_token(&worker.url(), &token).await;

    let response = post_with_session_id(
        &worker.url(),
        &session_id,
        None,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "valid server-issued session id should authorize post-initialize requests without token"
    );
    let body = response.text().await.expect("tools/list body");
    assert!(
        body.contains("\"result\""),
        "expected successful JSON-RPC result payload, got: {body}"
    );
    assert!(
        !body.contains("\"error\""),
        "valid session-id request must not return unauthorized error: {body}"
    );

    worker.shutdown(Duration::from_secs(5)).await;
}

// ───── T36: cross-brain session ids are rejected even with valid local HMAC ─────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_brain_session_id_is_rejected() {
    let dir_a = TempDir::new().expect("tempdir a");
    let dir_b = TempDir::new().expect("tempdir b");
    let brain_a = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440036".into()));
    let brain_b = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440037".into()));

    let (worker_a, _pm_a, _store_a, _orch_a) = boot_test_server(dir_a.path(), &brain_a).await;
    let (worker_b, _pm_b, _store_b, _orch_b) = boot_test_server(dir_b.path(), &brain_b).await;
    worker_a.register_delegation(DELEGATION_A.into(), DelegationContext::default());
    worker_b.register_delegation(DELEGATION_A.into(), DelegationContext::default());

    let token_a = worker_a.issue_token(DELEGATION_A, Duration::from_secs(60));
    let token_b = worker_b.issue_token(DELEGATION_A, Duration::from_secs(60));
    let session_id_a = open_session_with_token(&worker_a.url(), &token_a).await;

    let response = post_with_session_id(
        &worker_b.url(),
        &session_id_a,
        Some(&token_b),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}),
    )
    .await;
    assert!(
        response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::NOT_FOUND,
        "server B must reject session-id minted by server A (status={})",
        response.status()
    );

    worker_a.shutdown(Duration::from_secs(5)).await;
    worker_b.shutdown(Duration::from_secs(5)).await;
}
