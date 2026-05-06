use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::handlers::{get_issue, McpHandlerError, WorkerCallContext};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;

mod common;

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
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

#[tokio::test]
async fn get_issue_returns_issue_via_pm_service() {
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

#[tokio::test]
async fn get_issue_missing_id_param_returns_invalid_params() {
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
