//! Integration tests for disk-backed poll cursor.
//! Tests auto-skip if `br` is not installed.

use std::path::Path;
use std::process::Command;

use spur_pm::{BeadsAdapter, IssueTracker, PollCursor};
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
    if !br_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let cursor_file = dir.path().join(".spur-test-cursor");

    // First session: create one issue, poll it.
    {
        let adapter = BeadsAdapter::connect_with_actor(dir.path(), None, Some(cursor_file.clone()))
            .await
            .unwrap();
        run_br(dir.path(), &["create", "Issue1", "--silent", "-t", "task"]);
        let _ = adapter.poll().await.unwrap();
    }

    // Second session: open adapter with SAME cursor file. Poll should return
    // zero events (cursor persisted).
    {
        let adapter = BeadsAdapter::connect_with_actor(dir.path(), None, Some(cursor_file.clone()))
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

fn read_cursor_file(path: &Path) -> PollCursor {
    let s = std::fs::read_to_string(path).expect("cursor file readable");
    serde_json::from_str(&s).expect("cursor file is JSON PollCursor")
}

/// Regression test for the `--limit` saturation data-loss bug.
///
/// If `br list --limit N` returns exactly N items, more qualifying rows may
/// exist on the backend but were truncated. The prior "boundary-safe" cursor
/// advance unconditionally set `new_cursor.ts` to `max(fetched_subset.ts)`,
/// causing unfetched rows with `updated_at <= new_cursor.ts` to be silently
/// skipped forever.
///
/// Fix: when `items.len() == fetch_limit` (saturation) AND at least one row
/// was kept, preserve the prior cursor instead of advancing. Next poll
/// refetches and catches up.
///
/// Test uses the `poll_with_limit` inherent helper so saturation is
/// deterministic at a small N, without needing to create 500+ issues.
#[tokio::test]
async fn poll_does_not_advance_cursor_on_limit_saturation() {
    if !br_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let cursor_file = dir.path().join(".spur-test-cursor");

    let adapter = BeadsAdapter::connect_with_actor(dir.path(), None, Some(cursor_file.clone()))
        .await
        .unwrap();

    // Prime: create one issue and do a NON-saturating poll so we have a
    // concrete `prior_cursor` on disk (ts = Initial.updated_at).
    run_br(dir.path(), &["create", "Initial", "--silent", "-t", "task"]);
    let _ = adapter.poll_with_limit(10).await.unwrap();
    let cursor_before = read_cursor_file(&cursor_file);

    // Now create 6 more open issues. Together with "Initial" that's 7 open
    // rows — more than our test limit of 5.
    for i in 1..=6 {
        run_br(
            dir.path(),
            &["create", &format!("Issue{}", i), "--silent", "-t", "task"],
        );
    }

    // Saturated poll: limit=5 against 7 qualifying rows. Without the fix the
    // cursor advances to max(fetched_5.updated_at), silently dropping at
    // least one unfetched row on subsequent polls.
    let _ = adapter.poll_with_limit(5).await.unwrap();
    let cursor_after = read_cursor_file(&cursor_file);

    assert_eq!(
        cursor_before.ts, cursor_after.ts,
        "saturated poll must not advance cursor.ts (before={}, after={})",
        cursor_before.ts, cursor_after.ts
    );
    assert_eq!(
        cursor_before.ids_at_boundary, cursor_after.ids_at_boundary,
        "saturated poll must not change ids_at_boundary"
    );
}
