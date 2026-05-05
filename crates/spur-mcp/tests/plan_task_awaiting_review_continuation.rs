use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::domain::{BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, SessionId, SpurEventBody};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::McpEventSink;
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

#[derive(Default)]
struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEventBody>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events.lock().unwrap().push(body);
    }
}

fn extract_plan_id(response: &Value) -> String {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("submit_plan response text missing: {response}"));
    text.split("plan_id: ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap_or_else(|| panic!("plan_id missing from response text: {text}"))
        .to_string()
}

fn decode_tool_response(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "tool call should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text");
    serde_json::from_str(text).expect("tool response must be json")
}

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        panic!(
            "br {args:?} failed (exit {}): stderr={} stdout={}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

fn create_task_issue(repo: &Path, title: &str) -> String {
    let raw = run_br(
        repo,
        &[
            "create",
            "--type",
            "task",
            "--title",
            title,
            "--priority",
            "2",
            "--json",
        ],
    );
    let value: Value = serde_json::from_str(&raw).expect("br create json");
    value["id"].as_str().expect("issue id").to_string()
}

async fn beads_pm(repo: &Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn continuation_ctx(
    tx: tokio::sync::mpsc::UnboundedSender<BrainContinuation>,
) -> (DetachedContinuationCtx, Arc<Notify>) {
    let release = Arc::new(Notify::new());
    let release_ref = Arc::clone(&release);
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker_session| {
            let tx = tx.clone();
            let release = Arc::clone(&release_ref);
            Box::pin(async move {
                tx.send(cont).expect("capture continuation");
                release.notified().await;
            })
        }),
    };
    (ctx, release)
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn run_plan_pushes_continuation_and_event_for_awaiting_review_task() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let issue_id = create_task_issue(dir.path(), "Awaiting Review Continuation Task");
    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (continuation_ctx, release_continuation) = continuation_ctx(continuation_tx);
    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session = BrainSessionId::new(SessionId("brain-plan-review".into()));
    let (server, mut channel) = McpCallbackServer::new(
        Some(&session),
        Some(pm),
        Some(sink_ref),
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_submit_plan(json!({
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "finish this task",
                "issue_id": issue_id,
                "depends_on": []
            }]
        }))
        .await;
    let plan_id = extract_plan_id(&response);

    let request =
        tokio::time::timeout(std::time::Duration::from_secs(1), channel.request_rx.recv())
            .await
            .expect("plan task should dispatch")
            .expect("delegation request should be present");
    let delegation_id = request.id.to_string();
    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("ready for review".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("worker-branch".into()),
            artifact: None,
        })
        .expect("send worker result");

    let cont = tokio::time::timeout(std::time::Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("awaiting-review continuation should fire")
        .expect("continuation channel open");
    assert_eq!(cont.source, ContinuationSource::PlanTaskAwaitingReview);
    assert_eq!(cont.delegation_id.as_str(), delegation_id);

    let status = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
            .await,
    );
    assert_eq!(
        status["tasks"][0]["status"], "awaiting_review",
        "continuation must fire only after in-memory task status is terminal: {status}"
    );
    release_continuation.notify_waiters();

    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            SpurEventBody::PlanTaskAwaitingReview {
                plan_id: found_plan_id,
                task_id,
                delegation_id: found_delegation_id,
            } if found_plan_id == &plan_id
                && task_id == "t1"
                && found_delegation_id == &delegation_id
        )),
        "PlanTaskAwaitingReview event missing from {events:?}"
    );
}
