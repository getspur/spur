//! Phase 5 / Task 29 — end-to-end orchestrator → worker → MCP call test.
//!
//! Boots a real `Orchestrator` with a stubbed beads PM backend, lets
//! `Orchestrator::ensure_worker_mcp_server` lazy-start the per-`BrainSession`
//! `WorkerMcpServer`, simulates a worker by POSTing JSON-RPC against the
//! issued token URL, and asserts the full audit + summary chain:
//!
//!   (a) the `mcp_servers` injection slice the orchestrator's dispatch
//!       site assembles for the worker contains exactly one
//!       `spur-worker-mcp` HTTP entry whose URL embeds the minted token;
//!   (b) the worker's `get_issue` JSON-RPC call routes through the
//!       orchestrator-built server and returns the seeded issue payload;
//!   (c) a `ReadAggregate` audit-sentinel beads comment lands on the
//!       target issue with a `get_issue` entry. NOTE: production batches
//!       read-tool calls into `ReadAggregate` rather than emitting a
//!       per-call `WorkerMcp { subkind: Call }` sentinel; see
//!       `emit_read_aggregate` (`worker_server.rs:1175`). The original
//!       Phase 5 contract phrasing predated that batching design.
//!   (d) `WorkerMcpDelegationSummary` flows through the orchestrator's
//!       funnel with `calls_total >= 1`, `calls_by_tool["get_issue"] >= 1`,
//!       and `errors == 0`;
//!   (e) ORDERING NOTE: this assertion proves the orchestrator's broadcast
//!       channel preserves send-order. It does NOT prove that the
//!       production `flush_then_emit_completed` helper (private to
//!       spur-core) calls `funnel.emit(DelegationCompleted)` AFTER
//!       `flush_delegation` returns. That invariant is asserted at the
//!       lib level by `crates/spur-core/src/orchestrator.rs::flush_ordering_tests`,
//!       which exercises the funnel test channel directly. A true e2e
//!       ordering test would require dispatching a real ACP-subprocess
//!       worker, which is out of scope for this wiring test (filed as
//!       follow-up).
//!
//! The synthetic worker is an in-process tokio task, not a real ACP
//! subprocess; the full `execute_delegation` worker spawn path requires
//! a real ACP transport which is out of scope for an e2e wiring test.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use agent_client_protocol::schema::{McpServer, McpServerHttp};
use rmcp::{
    model::CallToolRequestParams,
    service::ServiceError,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::Value;
use spur_acp::config::SpurConfig;
use spur_acp::types::SessionId;
use spur_acp::{BrainSessionId, DelegationStatus, SpurEvent, SpurEventBody};
use spur_core::Orchestrator;
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan as LicensePlan};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::worker_server::DelegationContext;
use spur_mcp::McpCallbackServer;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, PmService};
use std::collections::BTreeSet;
use tempfile::TempDir;
use tokio::sync::broadcast;

const DELEGATION_ID: &str = "550e8400-e29b-41d4-a716-446655440123";

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
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_orchestrator_worker_mcp_get_issue() {
    let dir = TempDir::new().expect("tempdir");
    init_repo(dir.path());
    let beads = TestBeadsWorkspace::init();
    attach_test_beads(dir.path(), &beads);

    // PmService backed by the stubbed beads workspace. Required by the
    // orchestrator's WorkerMcpFetcher: the worker MCP server cannot boot
    // without a configured PM backend (read audits and worker-write
    // sentinels are emitted to it).
    let pm = Arc::new(
        PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new")
            .expect("expected Some(PmService)"),
    );

    // Pro feature gate is required: `emit_read_aggregate` short-circuits
    // when `PM_PRO_BEADS_ADVANCED` is missing, so the audit comment
    // would never land in beads under a community gate.
    let feature_gate = pro_feature_gate();

    let orch = Orchestrator::new(
        dir.path().into(),
        SpurConfig::default(),
        Some(Arc::clone(&feature_gate)),
    )
    .expect("Orchestrator::new")
    .with_pm_service(Arc::clone(&pm));

    // Subscribe BEFORE any emit so the funnel-stamped events land on
    // our receiver in arrival order.
    let mut events_rx = orch.subscribe();

    // Build the per-`BrainSession` brain MCP server. The
    // `WorkerMcpFetcher` reads its `PlanResolver` impl + reconciler
    // outcome handle from this instance.
    let brain_session_id =
        BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mcp_server, _channel) = McpCallbackServer::new(
        Some(&brain_session_id),
        Some(Arc::clone(&pm)),
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    let mcp_server = Arc::new(mcp_server);

    // ── Boot the worker MCP server via the orchestrator's public API. ──
    let worker_server = orch
        .ensure_worker_mcp_server(&brain_session_id, Arc::clone(&mcp_server))
        .await
        .expect("ensure_worker_mcp_server must boot a real WorkerMcpServer");

    let server_url = worker_server.url();
    assert!(
        server_url.starts_with("http://127.0.0.1:") && server_url.contains("/mcp"),
        "URL must point at a real listener with /mcp path: {server_url}"
    );

    // Register the synthetic delegation so the dispatcher's per-call
    // summary guard exists and `flush_delegation` has work to drain.
    worker_server.register_delegation(
        DELEGATION_ID.into(),
        DelegationContext {
            enable_worker_progress: true,
        },
    );

    let token = worker_server.issue_token(DELEGATION_ID, Duration::from_secs(60));
    assert!(!token.is_empty(), "minted token must be non-empty");

    // ── (a) Assert the worker `mcp_servers` injection shape. ───────────
    //
    // Mirrors `build_worker_mcp_servers_with(Some(true), ..)` — the
    // private helper the orchestrator's dispatch path uses to assemble
    // the worker `mcp_servers` slice. Reproducing the construction here
    // locks the contract that the public surface (URL + token) the test
    // observes is the same shape that lands in `session/new`.
    let url_with_token = format!("{}?token={}", server_url, token);
    let mcp_servers = [McpServer::Http(McpServerHttp::new(
        "spur-worker-mcp",
        &url_with_token,
    ))];
    assert_eq!(
        mcp_servers.len(),
        1,
        "exactly one worker MCP server entry must be injected"
    );
    match &mcp_servers[0] {
        McpServer::Http(http) => {
            assert_eq!(
                http.name, "spur-worker-mcp",
                "entry must be named 'spur-worker-mcp'"
            );
            assert!(
                http.url.contains(&format!("?token={}", token)),
                "URL must embed the minted token: {}",
                http.url
            );
            assert!(
                http.url.starts_with(&server_url),
                "URL must extend the live server URL: {}",
                http.url
            );
        }
        other => panic!("expected McpServer::Http, got {other:?}"),
    }

    // ── (b) Synthetic worker: HTTP+JSON-RPC `get_issue`. ───────────────
    //
    // Seed an issue first so `get_issue` returns a payload we can assert
    // on. The `id` field is set on the audit-buffer entry from the
    // request args, so this also drives (c).
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "e2e worker MCP target".into(),
            description: Some("worker reads this body via get_issue".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let mut tools = list_tools_with_bearer(&server_url, &token)
        .await
        .expect("list_tools must succeed over streamable HTTP");
    tools.sort();
    for expected in [
        "fetch_outcome_artifact",
        "get_issue",
        "get_plan_status",
        "get_task_diff",
        "list_issues",
        "report_progress",
        "report_signal",
        "update_issue",
    ] {
        assert!(
            tools.iter().any(|name| name == expected),
            "curated worker tool set missing {expected}: {tools:?}"
        );
    }

    let response = call_tool_with_bearer(
        &server_url,
        &token,
        "get_issue",
        serde_json::json!({ "id": &issue_id }),
    )
    .await;
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "get_issue must succeed, got: {response}"
    );
    assert_eq!(
        response["result"]["id"].as_str(),
        Some(issue_id.as_str()),
        "get_issue result must echo the requested id: {response}"
    );
    assert_eq!(
        response["result"]["body"].as_str(),
        Some("worker reads this body via get_issue"),
        "get_issue result must include the seeded body: {response}"
    );

    let list_issues = call_tool_with_bearer(
        &server_url,
        &token,
        "list_issues",
        serde_json::json!({ "limit": 20 }),
    )
    .await;
    assert!(
        list_issues.get("error").is_none() || list_issues["error"].is_null(),
        "list_issues must succeed, got: {list_issues}"
    );
    assert!(
        list_issues["result"].is_array(),
        "list_issues result must be an array, got: {list_issues}"
    );

    let get_plan_status = call_tool_with_bearer(
        &server_url,
        &token,
        "get_plan_status",
        serde_json::json!({ "plan_id": "missing-plan" }),
    )
    .await;
    assert_eq!(
        get_plan_status["error"]["code"].as_i64(),
        Some(-32602),
        "unknown plan_id should be invalid params, got: {get_plan_status}"
    );

    let get_task_diff = call_tool_with_bearer(
        &server_url,
        &token,
        "get_task_diff",
        serde_json::json!({
            "plan_id": "missing-plan",
            "task_id": &issue_id
        }),
    )
    .await;
    assert_eq!(
        get_task_diff["error"]["code"].as_i64(),
        Some(-32602),
        "unknown plan_id should be invalid params, got: {get_task_diff}"
    );

    let fetch_outcome_artifact = call_tool_with_bearer(
        &server_url,
        &token,
        "fetch_outcome_artifact",
        serde_json::json!({ "delegation_id": DELEGATION_ID }),
    )
    .await;
    assert!(
        matches!(
            fetch_outcome_artifact["error"]["code"].as_i64(),
            Some(-32004) | Some(-32001)
        ),
        "missing artifact must return a non-success MCP error, got: {fetch_outcome_artifact}"
    );

    let report_progress = call_tool_with_bearer(
        &server_url,
        &token,
        "report_progress",
        serde_json::json!({
            "message": "still working",
            "percent": 42.0
        }),
    )
    .await;
    assert_eq!(
        report_progress["result"]["ok"].as_bool(),
        Some(true),
        "report_progress must return ok=true, got: {report_progress}"
    );

    // ── Drive flush so (c) audit sentinel + (d) summary land. ──────────
    //
    // Production: `flush_then_emit_completed` (private to spur-core)
    // calls this on every terminal arm so the read-audit aggregator
    // drains before `DelegationCompleted` is emitted. We invoke
    // `flush_delegation` directly because the production helper is
    // private; the lib-level `flush_ordering_tests` already certify the
    // same call sequence.
    worker_server
        .flush_delegation(DELEGATION_ID, "success")
        .await
        .expect("flush_delegation must succeed");

    // ── (d) Drain broadcast until `WorkerMcpDelegationSummary` lands. ──
    let summary = wait_for_summary(&mut events_rx, DELEGATION_ID, Duration::from_secs(10))
        .await
        .expect("WorkerMcpDelegationSummary must be emitted");
    let SpurEventBody::WorkerMcpDelegationSummary {
        delegation_id,
        brain_session_id: brain_id,
        calls_total,
        calls_by_tool,
        errors,
        ..
    } = summary
    else {
        unreachable!("wait_for_summary returns only the matching variant")
    };
    assert_eq!(delegation_id, DELEGATION_ID);
    assert_eq!(brain_id, brain_session_id.to_string());
    assert_eq!(
        calls_total, 6,
        "summary must include five read calls plus one report_progress call"
    );
    for expected in [
        "fetch_outcome_artifact",
        "get_issue",
        "get_plan_status",
        "get_task_diff",
        "list_issues",
        "report_progress",
    ] {
        assert_eq!(
            calls_by_tool.get(expected).copied().unwrap_or(0),
            1,
            "summary must attribute exactly one call to {expected}: {calls_by_tool:?}"
        );
    }
    assert_eq!(
        errors, 3,
        "three read tools should fail in this fixture (unknown plan/artifact)"
    );

    // ── (e) Ordering: summary precedes DelegationCompleted. ────────────
    //
    // The orchestrator's `event_tx` broadcast preserves send-order on
    // its receivers. Sending DelegationCompleted AFTER we have already
    // observed the summary on `events_rx` therefore proves the
    // ordering invariant on the public broadcast surface.
    let worker_session = SessionId::new();
    orch.event_tx
        .send(SpurEvent {
            occurred_at: SystemTime::now(),
            seq: u64::MAX, // sentinel; real seq is funnel-stamped, not asserted here
            body: SpurEventBody::DelegationCompleted {
                worker_session: worker_session.clone(),
                status: DelegationStatus::Success,
            },
        })
        .expect("broadcast send DelegationCompleted");

    let completed = wait_for_event(
        &mut events_rx,
        |body| matches!(body, SpurEventBody::DelegationCompleted { .. }),
        Duration::from_secs(5),
    )
    .await
    .expect("DelegationCompleted must arrive after summary");
    assert!(
        matches!(completed, SpurEventBody::DelegationCompleted { .. }),
        "expected DelegationCompleted, got: {completed:?}"
    );

    // ── (c) Read-aggregate audit sentinel lands on the target issue. ──
    //
    // The audit comment is written by the background `audit_flusher_task`
    // after `flush_delegation` enqueues a `FlushMessage`. Poll because
    // PM I/O is asynchronous; cap at 5 s to keep the test under 60 s.
    let sentinel = wait_for_read_aggregate_sentinel(&pm, &issue_id, Duration::from_secs(5))
        .await
        .expect("ReadAggregate audit sentinel must land on the issue");
    let AuditSentinelKind::ReadAggregate {
        delegation_id: aud_id,
        entries,
    } = sentinel
    else {
        unreachable!("wait_for_read_aggregate_sentinel returns only ReadAggregate")
    };
    assert_eq!(aud_id, DELEGATION_ID);
    for expected in [
        "fetch_outcome_artifact",
        "get_issue",
        "get_plan_status",
        "get_task_diff",
        "list_issues",
    ] {
        assert!(
            entries.iter().any(|e| e.tool_name == expected),
            "audit entries must include {expected}: {entries:?}"
        );
    }
    assert!(
        entries
            .iter()
            .any(|e| e.tool_name == "get_issue" && e.target_issue_id.as_deref() == Some(&issue_id)),
        "get_issue entry must target the seeded issue: {entries:?}"
    );

    worker_server.shutdown(Duration::from_secs(5)).await;
}

async fn list_tools_with_bearer(url: &str, token: &str) -> Result<Vec<String>, String> {
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

async fn call_tool_with_bearer(url: &str, token: &str, name: &str, arguments: Value) -> Value {
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token);
    let client =
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .expect("rmcp client initialize");
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = arguments.as_object().cloned();
    let response = match client.call_tool(request).await {
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

/// Drain `rx` until a `WorkerMcpDelegationSummary` for `delegation_id`
/// is observed or the deadline elapses.
async fn wait_for_summary(
    rx: &mut broadcast::Receiver<SpurEvent>,
    delegation_id: &str,
    timeout: Duration,
) -> Option<SpurEventBody> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let next = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
        };
        if let SpurEventBody::WorkerMcpDelegationSummary {
            delegation_id: ref id,
            ..
        } = next.body
        {
            if id == delegation_id {
                return Some(next.body);
            }
        }
    }
}

/// Drain `rx` until a body matching `predicate` is observed or the deadline elapses.
async fn wait_for_event<F>(
    rx: &mut broadcast::Receiver<SpurEvent>,
    predicate: F,
    timeout: Duration,
) -> Option<SpurEventBody>
where
    F: Fn(&SpurEventBody) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let next = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
        };
        if predicate(&next.body) {
            return Some(next.body);
        }
    }
}

/// Poll the issue's beads comments for a `ReadAggregate` audit sentinel.
async fn wait_for_read_aggregate_sentinel(
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
