//! T19: Synchronous audit emission tests for worker write tools.
//!
//! Verifies that a successful `update_issue` call through the worker MCP
//! server emits a `[[spur-audit v1]] WorkerWrite` sentinel comment on the
//! target issue before returning success to the worker.

use std::path::Path;
use std::process::Command;
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
use spur_mcp::worker_server::{
    DelegationContext, ReadAuditBuffer, ReadAuditEntry, WorkerMcpDeps, WorkerMcpServer,
};
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
        funnel: Arc::new(NullSink),
        plan_resolver: Arc::new(NullPlanResolver),
        reconciler_outcomes: Arc::new(Mutex::new(
            spur_mcp::plan::outcomes::OutcomeStore::default(),
        )),
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

#[tokio::test]
async fn update_issue_success_emits_worker_write_audit_sentinel() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "audit emission test".into(),
            description: Some("issue body".into()),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let server = WorkerMcpServer::start("session-audit".into(), test_deps(Arc::clone(&pm)))
        .await
        .expect("start must succeed");
    let token = server.issue_token("d-1", Duration::from_secs(60));

    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "update_issue",
            "arguments": {
                "id": issue_id,
                "comment": "worker update"
            }
        }),
    )
    .await;

    assert_eq!(
        body["result"]["ok"].as_bool(),
        Some(true),
        "update_issue should return success, got: {body}"
    );

    // Verify the audit sentinel comment was written.
    let comments = pm
        .advanced()
        .expect("pm backend should expose advanced interface in test mode")
        .list_comments(&issue_id)
        .await
        .expect("list_comments");

    let sentinel = comments
        .iter()
        .find_map(|c| spur_mcp::plan::audit_sentinel::parse_comment(&c.body).and_then(|r| r.ok()));

    assert!(
        sentinel.is_some(),
        "expected audit sentinel comment, found: {comments:?}"
    );

    let sentinel = sentinel.unwrap();
    assert_eq!(sentinel.kind_str(), "worker-write");
    if let spur_mcp::plan::audit_sentinel::AuditSentinelKind::WorkerWrite {
        delegation_id,
        tool,
        issue_id: audited_issue_id,
    } = sentinel
    {
        assert_eq!(delegation_id, "d-1");
        assert_eq!(tool, "update_issue");
        assert_eq!(audited_issue_id, issue_id);
    } else {
        panic!("expected WorkerWrite sentinel, got: {sentinel:?}");
    }

    server.shutdown().await;
}

// ─── T20: per-delegation read-audit aggregation buffer ────────────────────

#[tokio::test]
async fn read_tool_calls_append_to_buffer() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
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

    server.shutdown().await;
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

#[tokio::test]
async fn update_issue_without_id_arg_does_not_emit_audit() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "no-id audit test".into(),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let server = WorkerMcpServer::start("session-audit".into(), test_deps(Arc::clone(&pm)))
        .await
        .expect("start must succeed");
    let token = server.issue_token("d-1", Duration::from_secs(60));

    let body = call_jsonrpc(
        &server,
        &token,
        "tools/call",
        serde_json::json!({
            "name": "update_issue",
            "arguments": { "comment": "no id" }
        }),
    )
    .await;

    assert!(
        body.get("error").is_some(),
        "expected error for missing id, got: {body}"
    );

    let comments = pm
        .advanced()
        .expect("advanced")
        .list_comments(&issue_id)
        .await
        .expect("list_comments");
    let sentinels: Vec<_> = comments
        .iter()
        .filter_map(|c| spur_mcp::plan::audit_sentinel::parse_comment(&c.body).and_then(|r| r.ok()))
        .collect();
    assert!(
        sentinels.is_empty(),
        "no audit sentinel should exist when id arg is missing, got: {sentinels:?}"
    );

    server.shutdown().await;
}
