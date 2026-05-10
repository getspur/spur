//! Integration coverage for submit_plan client idempotency keys.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_pm::{IssueFilter, PmService};
use tempfile::TempDir;

mod common;

fn run_git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    common::beads::run_br(repo, args).map(|_| ())
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn extract_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("response must include text content: {response}"))
}

fn extract_plan_id(response: &Value) -> String {
    let text = extract_text(response);
    text.split("plan_id: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("submit_plan response must include plan_id: {text}"))
        .to_string()
}

fn submit_args(key: &str) -> Value {
    json!({
        "client_idempotency_key": key,
        "persist_as_epic": true,
        "epic_title": "Idempotent Submit Epic",
        "tasks": [{
            "task_id": "t1",
            "agent": "codex",
            "task": "Do the idempotent task.",
            "depends_on": [],
            "context_files": []
        }]
    })
}

fn init_repo_with_beads() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]).expect("br init");
    dir
}

fn server(pm: Arc<PmService>, repo: &Path, session: &str) -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId(session.to_string()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(pm),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(repo.to_path_buf());
    server
}

#[tokio::test]
async fn submit_plan_client_idempotency_key_survives_brain_restart() {
    let dir = init_repo_with_beads();

    let key = "submit-plan-idempotent-key";
    let pm1 = beads_pm(dir.path()).await;
    let server1 = server(Arc::clone(&pm1), dir.path(), "brain-before-restart");
    let first = server1.__test_call_submit_plan(submit_args(key)).await;
    assert!(
        first.get("error").is_none(),
        "first submit_plan should succeed: {first}"
    );
    let first_plan_id = extract_plan_id(&first);
    assert!(
        !extract_text(&first).contains("idempotency key hit"),
        "first submit should not be reported as a dedup hit: {first}"
    );

    let pm2 = beads_pm(dir.path()).await;
    let server2 = server(Arc::clone(&pm2), dir.path(), "brain-after-restart");
    let second = server2
        .__test_call_submit_plan(json!({ "client_idempotency_key": key }))
        .await;
    assert!(
        second.get("error").is_none(),
        "second submit_plan should succeed: {second}"
    );
    let second_plan_id = extract_plan_id(&second);
    assert_eq!(
        second_plan_id, first_plan_id,
        "same client_idempotency_key must resolve to existing plan_id"
    );
    assert!(
        extract_text(&second).contains("idempotency key hit"),
        "second submit should report a dedup hit: {second}"
    );

    let dedup_epics = pm2
        .list_issues(IssueFilter {
            labels: vec!["spur:dedup".to_string()],
            include_closed: true,
            ..IssueFilter::default()
        })
        .await
        .expect("list dedup epics");
    assert_eq!(
        dedup_epics.len(),
        1,
        "dedup registry must be one synthetic epic"
    );

    let plan_epics = pm2
        .list_issues(IssueFilter {
            labels: vec![format!("spur:plan-id:{first_plan_id}")],
            include_closed: true,
            ..IssueFilter::default()
        })
        .await
        .expect("list persisted plan issues");
    let epic_count = plan_epics
        .iter()
        .filter(|issue| issue.issue_type.as_deref() == Some("epic"))
        .count();
    assert_eq!(
        epic_count, 1,
        "dedup hit must skip creating a second persisted plan epic"
    );
}

#[tokio::test]
async fn submit_plan_rejects_blank_client_idempotency_key() {
    let dir = init_repo_with_beads();
    let pm = beads_pm(dir.path()).await;
    let server = server(Arc::clone(&pm), dir.path(), "brain-blank-key");

    let response = server.__test_call_submit_plan(submit_args("   \t\n")).await;

    assert_eq!(
        response["error"]["message"], "submit_plan: client_idempotency_key must be non-empty",
        "blank client_idempotency_key should be rejected: {response}"
    );
}

#[tokio::test]
#[ignore = "pinned residual; closes in PR3"]
async fn submit_plan_record_failure_after_epic_build_orphans_plan_until_retry() {
    let dir = init_repo_with_beads();
    let db = dir.path().join(".beads/beads.db");
    let conn = Connection::open(&db).expect("open beads db");
    conn.execute_batch(
        "CREATE TRIGGER fail_submit_plan_dedup_record
         BEFORE INSERT ON issues
         WHEN NEW.title LIKE 'submit_plan dedup %'
         BEGIN
             SELECT RAISE(ABORT, 'injected dedup record failure');
         END;",
    )
    .expect("install injected dedup failure trigger");

    let key = "submit-plan-dedup-record-failure";
    let pm1 = beads_pm(dir.path()).await;
    let server1 = server(Arc::clone(&pm1), dir.path(), "brain-record-fail");
    let first = server1.__test_call_submit_plan(submit_args(key)).await;
    assert_eq!(
        first["error"]["code"], -32000,
        "record failure should be surfaced to caller: {first}"
    );

    conn.execute_batch("DROP TRIGGER fail_submit_plan_dedup_record;")
        .expect("remove injected dedup failure trigger");

    let pm2 = beads_pm(dir.path()).await;
    let server2 = server(Arc::clone(&pm2), dir.path(), "brain-record-retry");
    let retry = server2.__test_call_submit_plan(submit_args(key)).await;
    assert!(
        retry.get("error").is_none(),
        "retry after removing injected failure should succeed: {retry}"
    );

    let plan_epics = pm2
        .list_issues(IssueFilter {
            include_closed: true,
            ..IssueFilter::default()
        })
        .await
        .expect("list all issues");
    let idempotent_epic_count = plan_epics
        .iter()
        .filter(|issue| {
            issue.issue_type.as_deref() == Some("epic") && issue.title == "Idempotent Submit Epic"
        })
        .count();
    assert_eq!(
        idempotent_epic_count, 2,
        "retry after record failure currently creates a second persisted epic"
    );
}
