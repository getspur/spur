use std::path::Path;
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

/// Sink that blocks `emit()` for a chosen variant until the test releases it,
/// so a parallel test task can observe `PlanState` exactly at the moment the
/// event is being emitted. Used to detect event-vs-state ordering races.
struct InspectingSink {
    observed_tx: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    release_rx: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    captured: std::sync::Mutex<Vec<SpurEventBody>>,
}

impl McpEventSink for InspectingSink {
    fn emit(&self, body: SpurEventBody) {
        let is_terminal = matches!(
            body,
            SpurEventBody::PlanTaskFailed { .. } | SpurEventBody::PlanTaskAwaitingReview { .. }
        );
        self.captured.lock().unwrap().push(body);
        if is_terminal {
            if let Some(tx) = self.observed_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = self.release_rx.lock().unwrap().take() {
                let _ = rx.recv();
            }
        }
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

fn run_br(repo: &Path, args: &[&str]) -> String {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
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

#[tokio::test]
async fn run_plan_pushes_continuation_and_event_for_failed_task() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let issue_id = create_task_issue(dir.path(), "Failed Continuation Task");
    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (continuation_ctx, release_continuation) = continuation_ctx(continuation_tx);
    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session = BrainSessionId::new(SessionId("brain-plan-failed".into()));
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
                "task": "fail this task",
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
            status: DelegationStatus::Failed {
                error: "worker failed".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("worker failed".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send worker result");

    let cont = tokio::time::timeout(std::time::Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("failed task continuation should fire")
        .expect("continuation channel open");
    assert_eq!(cont.source, ContinuationSource::PlanTaskFailed);
    assert_eq!(cont.delegation_id.as_str(), delegation_id);

    let status = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
            .await,
    );
    assert_eq!(
        status["tasks"][0]["status"], "failed",
        "continuation must fire only after in-memory task status is terminal: {status}"
    );
    release_continuation.notify_waiters();

    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            SpurEventBody::PlanTaskFailed {
                plan_id: found_plan_id,
                task_id,
                attempt: 1,
                max_attempts: 3,
                error,
                delegation_id: found_delegation_id,
            } if found_plan_id == &plan_id
                && task_id == "t1"
                && error == "worker failed"
                && found_delegation_id == &delegation_id
        )),
        "PlanTaskFailed event missing from {events:?}"
    );
}

/// Race-detection test: when a worker observes the `PlanTaskFailed` event,
/// the in-memory plan state MUST already report the task as `failed`.
///
/// In v2 (cherry-pick `72fcec1d`) the persisted-completion path emits the
/// event from inside `persist_completion_inner` BEFORE `run_plan` acquires
/// the plan lock and writes the terminal status. This test holds emit() at a
/// rendezvous, queries `get_plan_status` while emit() is blocked, and asserts
/// the task is already `failed` at that moment. v2 fails this assertion (sees
/// `dispatched`); v3 — after moving the event emit into the deferred-push
/// `deliver()` step — passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_emission_does_not_race_with_state_update_for_failed_task() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let issue_id = create_task_issue(dir.path(), "Event Race Failed Task");

    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (continuation_ctx, release_continuation) = continuation_ctx(continuation_tx);

    let (observed_tx, observed_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let sink = Arc::new(InspectingSink {
        observed_tx: std::sync::Mutex::new(Some(observed_tx)),
        release_rx: std::sync::Mutex::new(Some(release_rx)),
        captured: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;

    let session = BrainSessionId::new(SessionId("brain-event-race-failed".into()));
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
                "task": "race detection task",
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
    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Failed {
                error: "race detection".into(),
            },
            diff: None,
            diff_summary: None,
            summary: Some("race".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("send worker result");

    // Wait for emit() to rendezvous on the terminal-event sink. While the
    // sender is blocked here, the worker thread is paused inside emit().
    tokio::task::spawn_blocking(move || {
        observed_rx
            .recv()
            .expect("PlanTaskFailed event should be observed");
    })
    .await
    .expect("observation task");

    // emit() is now blocked. Snapshot the plan status — if the v2 race is
    // present, it will read `dispatched`/`in_progress`. v3 must read `failed`.
    let status = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
            .await,
    );
    let observed_status = status["tasks"][0]["status"]
        .as_str()
        .unwrap_or("missing")
        .to_string();

    // Release emit() so the worker can proceed to push the continuation.
    release_tx.send(()).expect("release emit");

    let _cont = tokio::time::timeout(std::time::Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("continuation should fire after release")
        .expect("continuation channel open");
    release_continuation.notify_waiters();

    assert_eq!(
        observed_status, "failed",
        "PlanTaskFailed event must fire only after in-memory task status is terminal — observed: {observed_status}"
    );
}
