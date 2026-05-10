use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;

mod common;

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

async fn beads_pm(repo: &Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads-backed PmService"),
    )
}

fn advanced_submit_args() -> Value {
    json!({
        "persist_as_epic": true,
        "epic_title": "Advanced Gating Epic",
        "tasks": [{
            "task_id": "t1",
            "agent": "codex",
            "task": "Task requiring advanced beads persistence",
            "depends_on": [],
            "context_files": ["docs/advanced-gating.md"]
        }]
    })
}

#[tokio::test]
async fn submit_plan_persist_as_epic_proceeds_for_community_gate() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let (server, _channel) = common::server_builder::MockServerBuilder::community()
        .with_pm_service(pm)
        .build();

    let response = server
        .__test_call_tool("submit_plan", advanced_submit_args())
        .await;

    assert!(
        response.get("error").is_none(),
        "community gate should allow persisted submit_plan: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text response");
    assert!(text.contains("epic_id:"));
}

#[tokio::test]
async fn submit_plan_persist_as_epic_proceeds_for_pro_gate() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let (server, _channel) = common::server_builder::MockServerBuilder::pro()
        .with_pm_service(pm)
        .build();

    let response = server
        .__test_call_tool("submit_plan", advanced_submit_args())
        .await;

    assert!(
        response.get("error").is_none(),
        "pro gate should allow persisted submit_plan: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text response");
    assert!(text.contains("epic_id:"));
}
