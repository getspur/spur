//! T19: Synchronous audit emission tests for worker write tools.

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
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
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{
    DelegationContext, ReadAuditBuffer, ReadAuditEntry, WorkerMcpDeps, WorkerMcpServer,
    WorkerMcpServerConfig,
};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use tokio::sync::Mutex;

mod common;

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
    use std::collections::BTreeSet;
    let gate = FeatureGate::new(PolicyResolver::embedded());
    let pro_state =
        spur_license::LicenseState::active_validated(spur_license::Plan::Pro, BTreeSet::new());
    gate.update_state(&pro_state);
    Arc::new(gate)
}

struct NullSink;

impl McpEventSink for NullSink {
    fn emit(&self, _event: SpurEventBody) {}
}

#[derive(Default)]
struct RecordingSink {
    events: StdMutex<Vec<SpurEventBody>>,
}

impl RecordingSink {
    fn summary_count_for(&self, delegation_id: &str) -> usize {
        self.events
            .lock()
            .expect("recording sink lock")
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SpurEventBody::WorkerMcpDelegationSummary {
                        delegation_id: id,
                        ..
                    } if id == delegation_id
                )
            })
            .count()
    }
}

impl McpEventSink for RecordingSink {
    fn emit(&self, event: SpurEventBody) {
        self.events.lock().expect("recording sink lock").push(event);
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

async fn wait_for_read_aggregate_comment(
    pm: &PmService,
    issue_id: &str,
    timeout: Duration,
) -> Option<spur_mcp::plan::audit_sentinel::AuditSentinelKind> {
    let deadline = tokio::time::Instant::now() + timeout;
    let adv = pm.advanced()?;
    while tokio::time::Instant::now() < deadline {
        if let Ok(comments) = adv.list_comments(issue_id).await {
            for comment in comments {
                if let Some(Ok(kind)) = spur_mcp::plan::audit_sentinel::parse_comment(&comment.body)
                {
                    if matches!(
                        kind,
                        spur_mcp::plan::audit_sentinel::AuditSentinelKind::ReadAggregate { .. }
                    ) {
                        return Some(kind);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

// ─── T20: per-delegation read-audit aggregation buffer ────────────────────

#[tokio::test]
async fn read_tool_calls_append_to_buffer() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "read-buffer test".into(),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let server = WorkerMcpServer::start("session-read-buf".into(), test_deps(Arc::clone(&pm)))
        .await
        .expect("start must succeed");
    server.register_delegation(
        "d-1".into(),
        DelegationContext {
            enable_worker_progress: false,
        },
    );
    let token = server.issue_token("d-1", Duration::from_secs(60));

    for _ in 0..3 {
        let body = call_jsonrpc(
            &server,
            &token,
            "tools/call",
            serde_json::json!({
                "name": "get_issue",
                "arguments": { "id": issue_id }
            }),
        )
        .await;
        assert!(
            body.get("result").is_some(),
            "get_issue result; got: {body}"
        );
    }

    let buf = server
        .peek_read_buffer("d-1")
        .expect("buffer should exist after read calls");
    assert_eq!(buf.entry_count(), 3);

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn flush_delegation_drains_buffer_and_emits_summary_once() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "flush-delegation test".into(),
            description: Some("buffer drain target".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let sink = Arc::new(RecordingSink::default());
    let server = WorkerMcpServer::start(
        "session-flush-sync".into(),
        test_deps_with_funnel(Arc::clone(&pm), Arc::clone(&sink) as Arc<dyn McpEventSink>),
    )
    .await
    .expect("start must succeed");
    server.register_delegation(
        "d-sync".into(),
        DelegationContext {
            enable_worker_progress: false,
        },
    );
    let token = server.issue_token("d-sync", Duration::from_secs(60));

    for _ in 0..3 {
        let body = call_jsonrpc(
            &server,
            &token,
            "tools/call",
            serde_json::json!({
                "name": "get_issue",
                "arguments": { "id": &issue_id }
            }),
        )
        .await;
        assert!(
            body.get("error").is_none() || body["error"].is_null(),
            "get_issue should succeed, got: {body}"
        );
    }
    assert_eq!(
        server
            .peek_read_buffer("d-sync")
            .expect("buffer should exist")
            .entry_count(),
        3
    );

    server
        .flush_delegation("d-sync", "success")
        .await
        .expect("flush_delegation must succeed");
    assert!(
        server.peek_read_buffer("d-sync").is_none(),
        "flush_delegation should synchronously remove read buffer"
    );

    for _ in 0..100 {
        if sink.summary_count_for("d-sync") == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        sink.summary_count_for("d-sync"),
        1,
        "flush_delegation should emit exactly one summary event"
    );

    let summary = {
        let events = sink.events.lock().expect("recording sink lock");
        events.iter().find_map(|event| {
            if let SpurEventBody::WorkerMcpDelegationSummary {
                delegation_id,
                calls_total,
                calls_by_tool,
                errors,
                ..
            } = event
            {
                if delegation_id == "d-sync" {
                    return Some((*calls_total, calls_by_tool.clone(), *errors));
                }
            }
            None
        })
    };
    let (calls_total, calls_by_tool, errors) = summary.expect("summary event should exist");
    assert_eq!(
        calls_total, 3,
        "summary should include all three read calls"
    );
    assert_eq!(
        calls_by_tool.get("get_issue").copied().unwrap_or(0),
        3,
        "summary must attribute all calls to get_issue"
    );
    assert_eq!(errors, 0, "successful reads should not increment errors");
    let sentinel = wait_for_read_aggregate_comment(&pm, &issue_id, Duration::from_secs(5))
        .await
        .expect("read aggregate sentinel should be persisted");
    if let spur_mcp::plan::audit_sentinel::AuditSentinelKind::ReadAggregate {
        delegation_id,
        entries,
    } = sentinel
    {
        assert_eq!(delegation_id, "d-sync");
        assert_eq!(entries.len(), 3, "all three reads should be aggregated");
    } else {
        panic!("expected read aggregate sentinel");
    }

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn idle_flusher_drains_buffer_and_complete_emits_single_summary() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "idle-flusher test".into(),
            description: Some("idle flush target".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let sink = Arc::new(RecordingSink::default());
    let config = WorkerMcpServerConfig {
        idle_threshold: Duration::from_millis(100),
        scan_interval: Duration::from_millis(50),
    };
    let server = WorkerMcpServer::start_with_config(
        "session-flush-async".into(),
        test_deps_with_funnel(Arc::clone(&pm), Arc::clone(&sink) as Arc<dyn McpEventSink>),
        config,
    )
    .await
    .expect("start must succeed");
    server.register_delegation(
        "d-async".into(),
        DelegationContext {
            enable_worker_progress: false,
        },
    );
    let token = server.issue_token("d-async", Duration::from_secs(60));

    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "get_issue",
            "arguments": { "id": &issue_id }
        }),
    )
    .await;
    assert!(
        body.get("error").is_none() || body["error"].is_null(),
        "get_issue should succeed, got: {body}"
    );

    for _ in 0..100 {
        if server.peek_read_buffer("d-async").is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        server.peek_read_buffer("d-async").is_none(),
        "idle flusher should drain stale read buffer asynchronously"
    );

    server.complete_delegation("d-async", "success");

    for _ in 0..100 {
        if sink.summary_count_for("d-async") == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        sink.summary_count_for("d-async"),
        1,
        "complete_delegation should emit exactly one summary after idle flush"
    );

    let summary = {
        let events = sink.events.lock().expect("recording sink lock");
        events.iter().find_map(|event| {
            if let SpurEventBody::WorkerMcpDelegationSummary {
                delegation_id,
                calls_total,
                calls_by_tool,
                errors,
                ..
            } = event
            {
                if delegation_id == "d-async" {
                    return Some((*calls_total, calls_by_tool.clone(), *errors));
                }
            }
            None
        })
    };
    let (calls_total, calls_by_tool, errors) = summary.expect("summary event should exist");
    assert_eq!(calls_total, 1);
    assert_eq!(calls_by_tool.get("get_issue").copied().unwrap_or(0), 1);
    assert_eq!(errors, 0);
    let sentinel = wait_for_read_aggregate_comment(&pm, &issue_id, Duration::from_secs(5))
        .await
        .expect("idle flush should persist read aggregate sentinel");
    if let spur_mcp::plan::audit_sentinel::AuditSentinelKind::ReadAggregate {
        delegation_id,
        entries,
    } = sentinel
    {
        assert_eq!(delegation_id, "d-async");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name, "get_issue");
    } else {
        panic!("expected read aggregate sentinel");
    }

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn drop_buffer_sends_on_flush_channel() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let buf = ReadAuditBuffer::new("d-1".into(), tx);
    buf.append_for_test(ReadAuditEntry {
        tool_name: "get_issue".into(),
        target_issue_id: None,
        ts: 0,
    });
    drop(buf);
    let msg = rx.try_recv().expect("flush message expected on drop");
    assert_eq!(msg.delegation_id, "d-1");
    assert_eq!(msg.entries.len(), 1);
    assert_eq!(msg.entries[0].tool_name, "get_issue");
}

#[tokio::test]
async fn drop_buffer_with_no_entries_is_silent() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let buf = ReadAuditBuffer::new("d-2".into(), tx);
    drop(buf);
    assert!(
        rx.try_recv().is_err(),
        "empty buffer must NOT send a flush message"
    );
}

#[tokio::test]
async fn drop_buffer_after_receiver_dropped_does_not_panic() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    let buf = ReadAuditBuffer::new("d-3".into(), tx);
    buf.append_for_test(ReadAuditEntry {
        tool_name: "list_issues".into(),
        target_issue_id: None,
        ts: 1,
    });
    // Must not panic even though the receiver is gone.
    drop(buf);
}
