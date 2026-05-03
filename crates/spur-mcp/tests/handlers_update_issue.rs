use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::handlers::{update_issue, McpHandlerError, WorkerCallContext};
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

async fn test_pm_service_empty(repo: &Path) -> Arc<PmService> {
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
async fn update_issue_writes_comment_via_pm() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let issue_id = create_issue(&pm, "Issue title", "issue body").await;
    let args = json!({
        "id": issue_id.clone(),
        "comment": "hello from worker"
    });
    let ctx = WorkerCallContext {
        delegation_id: "d".into(),
        brain_session_id: "b".into(),
    };

    let result = update_issue(&pm, &ctx, args).await.unwrap();
    assert_eq!(result["ok"], true);

    let comments = pm
        .advanced()
        .expect("pm backend should expose advanced interface in test mode")
        .list_comments(&issue_id)
        .await
        .expect("list_comments");

    assert!(
        comments
            .iter()
            .any(|comment| comment.body.contains("hello from worker")),
        "comment should be written via pm.update_issue"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn update_issue_missing_id_invalid_params() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let ctx = WorkerCallContext {
        delegation_id: "d".into(),
        brain_session_id: "b".into(),
    };

    let err = update_issue(&pm, &ctx, json!({"comment": "x"}))
        .await
        .expect_err("missing id should produce InvalidParams");

    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn update_issue_propagates_priority_and_assignee() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let issue_id = create_issue(&pm, "Test issue", "body").await;
    let args = json!({
        "id": issue_id.clone(),
        "priority": 2,
        "assignee": "alice"
    });
    let ctx = WorkerCallContext {
        delegation_id: "d".to_string(),
        brain_session_id: "b".to_string(),
    };

    update_issue(&pm, &ctx, args)
        .await
        .expect("update_issue should succeed");

    let issue = pm.get_issue(&issue_id).await.expect("get_issue");
    assert_eq!(
        issue.priority,
        Some(2),
        "priority must propagate from JSON args into IssueUpdate (bd-1u8p-style regression check)"
    );
    assert_eq!(
        issue.assignee.as_deref(),
        Some("alice"),
        "assignee must propagate from JSON args into IssueUpdate (bd-1u8p-style regression check)"
    );
}
