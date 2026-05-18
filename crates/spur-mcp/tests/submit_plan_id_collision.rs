mod common;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::time::timeout;

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

struct SubmitFixture {
    _dir: TempDir,
    pm: Arc<PmService>,
    server: McpCallbackServer,
}

async fn submit_fixture(session: &str) -> SubmitFixture {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]).expect("br init");

    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId(session.to_string()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    SubmitFixture {
        _dir: dir,
        pm,
        server,
    }
}

fn submit_args(plan_name: &str, child_count: usize) -> Value {
    let tasks: Vec<Value> = (0..child_count)
        .map(|idx| {
            json!({
                "task_id": format!("tokens-{idx:03}"),
                "agent": "codex",
                "task": format!("Tokens child task with shared prefix {idx:03}"),
                "depends_on": [],
                "context_files": []
            })
        })
        .collect();

    json!({
        "persist_as_epic": true,
        "epic_title": format!("ID Collision Regression {plan_name}"),
        "tasks": tasks
    })
}

fn response_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("response must include text content: {response}"))
}

fn assert_submit_ok(response: &Value) {
    if let Some(error) = response.get("error") {
        let rendered = error.to_string();
        assert!(
            !rendered.contains("UNIQUE constraint failed: issues.id"),
            "submit_plan surfaced an ID collision: {response}"
        );
        panic!("submit_plan should succeed: {response}");
    }
}

fn extract_task_map(response: &Value) -> HashMap<String, String> {
    let task_map_json = response_text(response)
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .unwrap_or_else(|| panic!("submit_plan response must include task_map: {response}"));
    serde_json::from_str(task_map_json).expect("task_map line must be valid JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_plan_persists_many_children_without_id_collision() {
    let fixture = submit_fixture("many-children").await;

    let response = fixture
        .server
        .__test_call_submit_plan(submit_args("many", 50))
        .await;

    assert_submit_ok(&response);
    let task_map = extract_task_map(&response);
    assert_eq!(task_map.len(), 50);
    let unique: HashSet<_> = task_map.values().collect();
    assert_eq!(unique.len(), task_map.len(), "child IDs must be distinct");

    for issue_id in task_map.values() {
        fixture
            .pm
            .get_issue(issue_id)
            .await
            .unwrap_or_else(|_| panic!("persisted child {issue_id} should be fetchable"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submit_plan_no_id_collision() {
    let fixture = submit_fixture("concurrent-submit").await;

    let responses = timeout(Duration::from_secs(10), async {
        let calls = (0..4).map(|idx| {
            let args = submit_args(&format!("concurrent-{idx}"), 8);
            fixture.server.__test_call_submit_plan(args)
        });
        futures::future::join_all(calls).await
    })
    .await
    .expect("concurrent submit_plan calls should finish within 10s");

    let mut all_child_ids = HashSet::new();
    for response in responses {
        assert_submit_ok(&response);
        let task_map = extract_task_map(&response);
        assert_eq!(task_map.len(), 8);
        for issue_id in task_map.into_values() {
            assert!(
                all_child_ids.insert(issue_id),
                "concurrent submit_plan child IDs must be globally distinct"
            );
        }
    }
}
