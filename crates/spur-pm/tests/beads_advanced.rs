//! Integration tests for `BeadsAdvanced`. Each test spins up a temp `.beads/`
//! workspace and shells out to the real `br` binary. Tests auto-skip if `br`
//! is not installed.

use std::path::Path;
use std::process::Command;

use spur_pm::{BeadsAdapter, BeadsAdvanced, ReadyFilter};
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

async fn setup_workspace() -> (TempDir, BeadsAdapter) {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let adapter = BeadsAdapter::connect(dir.path())
        .await
        .expect("connect failed");
    (dir, adapter)
}

#[tokio::test]
async fn list_ready_returns_unblocked_issues() {
    if !br_available() {
        eprintln!("skipping: `br` binary not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;

    // Create two tasks with a dependency: A blocks B. Only A is ready.
    let a = run_br(dir.path(), &["create", "Task A", "--silent", "-t", "task"])
        .trim()
        .to_string();
    let b = run_br(dir.path(), &["create", "Task B", "--silent", "-t", "task"])
        .trim()
        .to_string();
    // Wait a sec — `br create` returns just the ID with --silent.
    let a_id = a.trim_matches('"').to_string();
    let b_id = b.trim_matches('"').to_string();
    run_br(dir.path(), &["dep", "add", &b_id, &a_id]);

    let ready = adapter
        .list_ready(ReadyFilter {
            limit: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();

    let ids: Vec<String> = ready.into_iter().map(|i| i.id).collect();
    assert!(
        ids.contains(&a_id),
        "expected A ({a_id}) in ready, got {ids:?}"
    );
    assert!(!ids.contains(&b_id), "B ({b_id}) should be blocked");
}

#[tokio::test]
async fn add_comment_then_list_returns_it() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    let id_raw = run_br(dir.path(), &["create", "T", "--silent", "-t", "task"]);
    let id = id_raw.trim().trim_matches('"').to_string();

    let cid = adapter.add_comment(&id, "hello world").await.unwrap();
    assert!(!cid.is_empty(), "expected non-empty comment id");

    let comments = adapter.list_comments(&id).await.unwrap();
    assert!(
        comments.iter().any(|c| c.body.contains("hello world")),
        "expected comment with body 'hello world', got {comments:?}"
    );
}

#[tokio::test]
async fn remove_dependency_unblocks_task() {
    if !br_available() {
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    let a = run_br(dir.path(), &["create", "A", "--silent", "-t", "task"])
        .trim()
        .trim_matches('"')
        .to_string();
    let b = run_br(dir.path(), &["create", "B", "--silent", "-t", "task"])
        .trim()
        .trim_matches('"')
        .to_string();
    run_br(dir.path(), &["dep", "add", &b, &a]);

    adapter.remove_dependency(&b, &a).await.unwrap();

    let ready = adapter
        .list_ready(ReadyFilter {
            limit: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();
    let ids: Vec<String> = ready.into_iter().map(|i| i.id).collect();
    assert!(
        ids.contains(&b),
        "B should be ready after dep removed, got {ids:?}"
    );
}

#[tokio::test]
async fn dep_cycles_detects_cycle() {
    if !br_available() {
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    let a = run_br(dir.path(), &["create", "A", "--silent", "-t", "task"])
        .trim()
        .trim_matches('"')
        .to_string();
    let b = run_br(dir.path(), &["create", "B", "--silent", "-t", "task"])
        .trim()
        .trim_matches('"')
        .to_string();
    run_br(dir.path(), &["dep", "add", &a, &b]); // A blocks on B
                                                 // Try to create a cycle: B blocks on A. `br` may reject at add time;
                                                 // if so, the cycle never exists and dep_cycles should return empty.
    let maybe_cycle = Command::new("br")
        .args(["dep", "add", &b, &a, "--json"])
        .current_dir(dir.path())
        .output()
        .expect("br invocation");

    let cycles = adapter.dep_cycles().await.unwrap();
    if maybe_cycle.status.success() {
        // br allowed the cycle; detector must find it.
        assert!(!cycles.is_empty(), "expected cycle, got {cycles:?}");
    } else {
        // br rejected the cycle; detector should find none.
        assert!(
            cycles.is_empty(),
            "no cycle but detector returned {cycles:?}"
        );
    }
}

/// B1+B2 regression: `ReadyFilter.priorities` is a set-membership filter
/// matching br's actual `-p` semantics. Empirically: `br ready -p 0 -p 2`
/// returns P0 ∪ P2, not a range.
#[tokio::test]
async fn list_ready_priorities_is_set_membership() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;

    // Create issues at priorities 0, 2, 4. Use `-p` on create to set initial priority.
    run_br(
        dir.path(),
        &["create", "P0", "--silent", "-t", "task", "-p", "0"],
    );
    run_br(
        dir.path(),
        &["create", "P2", "--silent", "-t", "task", "-p", "2"],
    );
    run_br(
        dir.path(),
        &["create", "P4", "--silent", "-t", "task", "-p", "4"],
    );

    // Filter to {0, 2}. Expect exactly two issues, priorities 0 and 2.
    let ready = adapter
        .list_ready(ReadyFilter {
            priorities: vec![0, 2],
            limit: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();

    let priorities: Vec<i32> = ready
        .iter()
        .map(|i| i.priority.expect("br ready row must carry priority"))
        .collect();

    assert_eq!(priorities.len(), 2, "expected 2 items, got {priorities:?}");
    assert!(
        priorities.contains(&0),
        "missing priority 0: {priorities:?}"
    );
    assert!(
        priorities.contains(&2),
        "missing priority 2: {priorities:?}"
    );
    assert!(
        !priorities.contains(&4),
        "priority 4 should be filtered out: {priorities:?}"
    );
}

/// Empty `priorities` means no priority filter — returns all ready items.
#[tokio::test]
async fn list_ready_empty_priorities_means_no_filter() {
    if !br_available() {
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    run_br(
        dir.path(),
        &["create", "A", "--silent", "-t", "task", "-p", "0"],
    );
    run_br(
        dir.path(),
        &["create", "B", "--silent", "-t", "task", "-p", "4"],
    );

    let ready = adapter
        .list_ready(ReadyFilter {
            priorities: vec![],
            limit: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ready.len(), 2, "expected both issues with no filter");
}
