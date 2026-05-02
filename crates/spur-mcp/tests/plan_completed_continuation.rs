use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::domain::{BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, SessionId, SpurEventBody};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::McpEventSink;

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

fn continuation_ctx(
    tx: tokio::sync::mpsc::UnboundedSender<BrainContinuation>,
) -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(move |cont, _worker_session| {
            let tx = tx.clone();
            Box::pin(async move {
                tx.send(cont).expect("capture continuation");
            })
        }),
    }
}

async fn complete_next_request(channel: &mut spur_mcp::DelegationChannel, summary: &str) -> String {
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
            summary: Some(summary.into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some(format!("{summary}-branch")),
            artifact: None,
        })
        .expect("send worker result");
    delegation_id
}

async fn approve_task(server: &McpCallbackServer, plan_id: &str, task_id: &str) {
    let response = server
        .__test_call_tool(
            "review_task",
            json!({
                "plan_id": plan_id,
                "task_id": task_id,
                "decision": "approve",
                "feedback": "ok"
            }),
        )
        .await;
    assert!(
        response.get("error").is_none(),
        "review_task should succeed: {response}"
    );
}

#[tokio::test]
async fn run_plan_pushes_continuation_when_plan_completes() {
    let (continuation_tx, mut continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session = BrainSessionId::new(SessionId("brain-plan-completed".into()));
    let (server, mut channel) = McpCallbackServer::new(
        &session,
        None,
        Some(sink_ref),
        continuation_ctx(continuation_tx),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_submit_plan(json!({
            "tasks": [
                {
                    "task_id": "t1",
                    "agent": "codex",
                    "task": "finish first task",
                    "depends_on": []
                },
                {
                    "task_id": "t2",
                    "agent": "codex",
                    "task": "finish second task",
                    "depends_on": []
                }
            ]
        }))
        .await;
    let plan_id = extract_plan_id(&response);

    complete_next_request(&mut channel, "first").await;
    let first_cont =
        tokio::time::timeout(std::time::Duration::from_secs(1), continuation_rx.recv())
            .await
            .expect("first task continuation should fire")
            .expect("continuation channel open");
    assert_eq!(
        first_cont.source,
        ContinuationSource::PlanTaskAwaitingReview
    );
    approve_task(&server, &plan_id, "t1").await;

    complete_next_request(&mut channel, "second").await;
    let second_cont =
        tokio::time::timeout(std::time::Duration::from_secs(1), continuation_rx.recv())
            .await
            .expect("second task continuation should fire")
            .expect("continuation channel open");
    assert_eq!(
        second_cont.source,
        ContinuationSource::PlanTaskAwaitingReview
    );
    approve_task(&server, &plan_id, "t2").await;

    let plan_cont = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let cont = continuation_rx
                .recv()
                .await
                .expect("continuation channel open");
            if cont.source == ContinuationSource::PlanCompleted {
                break cont;
            }
        }
    })
    .await
    .expect("PlanCompleted continuation should fire");
    assert_eq!(plan_cont.source, ContinuationSource::PlanCompleted);

    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            SpurEventBody::PlanCompleted {
                plan_id: found_plan_id,
                approved: 2,
                ..
            } if found_plan_id == &plan_id
        )),
        "PlanCompleted event missing from {events:?}"
    );
}
