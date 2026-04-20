//! Integration tests for disk-backed poll cursor.
//! Tests auto-skip if `br` is not installed.

use std::path::Path;
use std::process::Command;

use spur_pm::{BeadsAdapter, IssueTracker};
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

#[tokio::test]
async fn disk_cursor_survives_adapter_restart() {
    if !br_available() { return; }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let cursor_file = dir.path().join(".spur-test-cursor");

    // First session: create one issue, poll it.
    {
        let adapter = BeadsAdapter::connect_with_actor(
            dir.path(),
            None,
            Some(cursor_file.clone()),
        )
        .await
        .unwrap();
        run_br(dir.path(), &["create", "Issue1", "--silent", "-t", "task"]);
        let _ = adapter.poll().await.unwrap();
    }

    // Second session: open adapter with SAME cursor file. Poll should return
    // zero events (cursor persisted).
    {
        let adapter = BeadsAdapter::connect_with_actor(
            dir.path(),
            None,
            Some(cursor_file.clone()),
        )
        .await
        .unwrap();
        let events = adapter.poll().await.unwrap();
        assert!(
            events.is_empty(),
            "second session saw {} stale events — disk cursor not persisted",
            events.len()
        );
    }
}
