use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use tempfile::TempDir;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        out.status.success(),
        "git {args:?} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    assert!(
        out.status.success(),
        "br {args:?} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn extract_submit_plan_task_issue_id(response: &serde_json::Value, task_id: &str) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_line = text
        .lines()
        .find(|line| line.trim_start().starts_with("task_map: "))
        .expect("submit_plan response must include task_map line");
    let task_map: std::collections::HashMap<String, String> =
        serde_json::from_str(task_map_line.trim_start().trim_start_matches("task_map: "))
            .expect("task_map must be JSON");
    task_map
        .get(task_id)
        .cloned()
        .unwrap_or_else(|| panic!("task_map must include '{task_id}'"))
}

#[tokio::test]
async fn reconciler_starts_only_after_brain_session_id_is_bound() {
    if !br_available() {
        eprintln!(
            "skipping reconciler_starts_only_after_brain_session_id_is_bound: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("reconciler_starts_only_after_brain_session_id_is_bound");

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]);

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected beads pm");
    let pm = Arc::new(pm);
    let (mut server, mut channel) = McpCallbackServer::new(
        None,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);

    let server = Arc::new(server);
    let (_url, mcp_handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start must not require brain_session_id when reconciler is configured");
    assert!(
        channel.request_rx.try_recv().is_err(),
        "listener-only start must not dispatch before the reconciler is enabled"
    );
    assert!(
        !server.__test_reconciler_running(),
        "start must not spawn the reconciler before enable_reconciler"
    );

    let brain_sid = BrainSessionId::new(SessionId("brain".into()));
    server
        .set_brain_session_id(brain_sid)
        .expect("brain_session_id set once");

    let response = server
        .__test_call_submit_plan(serde_json::json!({
            "persist_as_epic": true,
            "epic_title": "Late Enabled Reconciler Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Dispatch after the reconciler is explicitly enabled",
                "depends_on": [],
            }]
        }))
        .await;
    let task_id = extract_submit_plan_task_issue_id(&response, "t1");

    assert!(
        tokio::time::timeout(Duration::from_millis(150), channel.request_rx.recv())
            .await
            .is_err(),
        "ready persisted work must not dispatch before enable_reconciler"
    );

    Arc::clone(&server)
        .enable_reconciler()
        .await
        .expect("enable reconciler after binding brain_session_id");
    assert!(
        server.__test_reconciler_running(),
        "enable_reconciler should spawn the reconciler after brain_session_id is bound"
    );

    let request = tokio::time::timeout(Duration::from_secs(5), channel.request_rx.recv())
        .await
        .expect("enabled reconciler should dispatch ready work within timeout")
        .expect("dispatch request");

    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));

    mcp_handle.abort();
    let _ = mcp_handle.await;
}
