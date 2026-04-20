//! Integration tests for `PmService` on the beads backend.
//! Tests auto-skip if `br` is not installed.

use std::path::Path;
use std::process::Command;

use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .output()
        .expect("br invocation failed");
    assert!(out.status.success(), "br {:?} failed: {:?}", args, out);
    String::from_utf8(out.stdout).unwrap()
}

#[derive(serde::Deserialize)]
struct BrShowIssue {
    #[serde(default)]
    created_by: Option<String>,
}

#[tokio::test]
async fn pm_service_poll_persists_cursor_across_restarts() {
    if !br_available() {
        return;
    }

    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    run_br(dir.path(), &["create", "Issue1", "--silent", "-t", "task"]);

    let first = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");
    let first_events = first.poll().await.unwrap();
    assert_eq!(
        first_events.len(),
        1,
        "first session must see the seeded issue"
    );

    let second = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");
    let second_events = second.poll().await.unwrap();
    assert!(
        second_events.is_empty(),
        "second session replayed already-polled events: {second_events:?}"
    );
}

#[tokio::test]
async fn pm_service_create_issue_uses_reconciler_actor_by_default() {
    if !br_available() {
        return;
    }

    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);

    let service = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");

    let issue_id = service
        .create_issue(IssueCreate {
            title: "Actor threaded create".to_string(),
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let shown: Vec<BrShowIssue> =
        serde_json::from_str(&run_br(dir.path(), &["show", &issue_id])).unwrap();
    let created_by = shown
        .first()
        .and_then(|issue| issue.created_by.as_deref())
        .expect("br show must expose created_by");
    assert_eq!(created_by, "reconciler");
}
