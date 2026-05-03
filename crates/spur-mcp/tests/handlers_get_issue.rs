use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::handlers::{get_issue, McpHandlerError, WorkerCallContext};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;

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
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
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

async fn create_issue(pm: &PmService, title: &str, body: &str) -> String {
    pm.create_issue(IssueCreate {
        title: title.to_string(),
        description: Some(body.to_string()),
        issue_type: Some("task".to_string()),
        ..Default::default()
    })
    .await
    .expect("create issue")
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_issue_returns_issue_via_pm_service() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = create_issue(&pm, "Issue title", "test issue body").await;
    let args = json!({ "id": issue_id.clone() });
    let ctx = WorkerCallContext {
        delegation_id: "d-1".to_string(),
        brain_session_id: "b-1".to_string(),
    };

    let value = get_issue(&pm, &ctx, args)
        .await
        .expect("get_issue should return issue");

    assert_eq!(value["id"].as_str(), Some(issue_id.as_str()));
    assert_eq!(value["body"].as_str(), Some("test issue body"));
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_issue_missing_id_param_returns_invalid_params() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = pm_service_fixture(dir.path()).await;
    let args = json!({});
    let ctx = WorkerCallContext {
        delegation_id: "d-1".to_string(),
        brain_session_id: "b-1".to_string(),
    };

    let err = get_issue(&pm, &ctx, args)
        .await
        .expect_err("get_issue should return invalid params error");
    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
}
