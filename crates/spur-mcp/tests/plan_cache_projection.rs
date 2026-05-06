use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_pm::{IssueUpdate, PmService};
use tempfile::TempDir;

mod common;

fn br_available() -> bool {
    common::beads::br_available()
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

fn extract_submit_plan_id(response: &Value) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    text.lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .map(str::to_string)
        .expect("submit_plan response must include plan_id line")
}

fn extract_submit_plan_task_issue_id(response: &Value, task_id: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_json = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .expect("submit_plan response must include task_map line");
    let task_map: std::collections::HashMap<String, String> =
        serde_json::from_str(task_map_json).expect("task_map line must be valid JSON");
    task_map
        .get(task_id)
        .cloned()
        .unwrap_or_else(|| panic!("submit_plan task_map must include '{task_id}'"))
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_plan_status_reprojects_persisted_plan_instead_of_trusting_corrupted_cache() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(pm),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let submit_response = server
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "Projection Harness Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Projection Harness Task",
                "depends_on": [],
                "context_files": ["docs/harness.md"]
            }]
        }))
        .await;
    let plan_id = extract_submit_plan_id(&submit_response);

    let baseline = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id.clone() }))
            .await,
    );
    assert_ne!(
        baseline["tasks"][0]["status"], "approved",
        "baseline status must differ from the injected corruption"
    );

    server
        .__test_corrupt_cached_plan(&plan_id, "t1", "spur/bogus-worker", "spur/bogus-snapshot")
        .await
        .expect("corrupt cached plan");

    let refreshed = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
            .await,
    );

    assert_eq!(
        refreshed, baseline,
        "get_plan_status must rebuild persisted state instead of trusting corrupted cache"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_plan_status_preserves_in_progress_persisted_children() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let submit_response = server
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "Projection Coverage Epic",
            "tasks": [
                {
                    "task_id": "t1",
                    "agent": "codex",
                    "task": "Active task",
                    "depends_on": [],
                    "context_files": []
                },
                {
                    "task_id": "t2",
                    "agent": "codex",
                    "task": "Blocked on t1",
                    "depends_on": ["t1"],
                    "context_files": []
                }
            ]
        }))
        .await;
    let plan_id = extract_submit_plan_id(&submit_response);
    let task_issue_id = extract_submit_plan_task_issue_id(&submit_response, "t1");

    pm.update_issue(
        &task_issue_id,
        IssueUpdate {
            status: Some("in_progress".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("mark task in_progress");
    spur_mcp::plan::persist_dispatch_intent(
        pm.as_ref(),
        &task_issue_id,
        common::server_builder::pro_feature_gate().as_ref(),
        &plan_id,
        "del-inflight",
        "codex",
        1,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("persist dispatch intent");

    let projected = decode_tool_response(
        &server
            .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
            .await,
    );
    let tasks = projected["tasks"].as_array().expect("tasks array");

    assert_eq!(
        tasks.len(),
        2,
        "persisted projection must keep in_progress child tasks"
    );
    assert!(
        tasks
            .iter()
            .any(|task| { task["task_id"] == "t1" && task["status"] == "dispatched" }),
        "active child must still appear as dispatched: {projected}"
    );
    assert!(
        tasks
            .iter()
            .any(|task| task["task_id"] == "t2" && task["status"] == "pending"),
        "dependent sibling must remain visible after re-projection: {projected}"
    );
}
