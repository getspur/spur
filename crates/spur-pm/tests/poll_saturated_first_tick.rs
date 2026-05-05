//! Regression test for first-tick saturation with no prior cursor.
//! Tests auto-skip if `br` is not installed.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use spur_pm::{beads::POLL_FETCH_LIMIT, BeadsAdapter, IssueTracker, PmEvent};
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

fn event_ids(events: &[PmEvent]) -> HashSet<String> {
    events
        .iter()
        .map(|event| match event {
            PmEvent::IssueCreated(summary) | PmEvent::IssueUpdated(summary) => summary.id.clone(),
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct BrIssueRow {
    id: String,
}

#[derive(serde::Deserialize)]
struct BrListOutput {
    issues: Vec<BrIssueRow>,
}

#[tokio::test]
async fn saturated_first_poll_keeps_cursor_unset_until_backlog_drains() {
    if !br_available() {
        return;
    }

    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let cursor_file = dir.path().join(".spur-test-cursor");

    for i in 0..(POLL_FETCH_LIMIT + 5) {
        run_br(
            dir.path(),
            &["create", &format!("Issue {i}"), "--silent", "-t", "task"],
        );
    }

    let all_rows: BrListOutput = serde_json::from_str(&run_br(
        dir.path(),
        &["list", "-s", "open", "--limit", "600"],
    ))
    .expect("br list output must be valid JSON");
    let all_rows = all_rows.issues;
    assert_eq!(all_rows.len(), POLL_FETCH_LIMIT + 5);

    let adapter = BeadsAdapter::connect_with_actor(dir.path(), None, Some(cursor_file.clone()))
        .await
        .unwrap();

    let first_events = adapter.poll().await.unwrap();
    assert_eq!(first_events.len(), POLL_FETCH_LIMIT);
    assert!(
        !cursor_file.exists(),
        "first saturated poll without a prior cursor must leave the cursor unset"
    );

    let first_ids = event_ids(&first_events);
    let unfetched_ids: HashSet<String> = all_rows
        .iter()
        .map(|row| row.id.clone())
        .filter(|id| !first_ids.contains(id))
        .collect();
    assert_eq!(
        unfetched_ids.len(),
        5,
        "expected the first saturated poll to leave exactly 5 rows unfetched"
    );

    // Remove the head batch from the ready set. With the buggy `Utc::now()`
    // cursor these older rows stay permanently hidden; with the fix they
    // remain eligible and appear on the next poll.
    let mut close_args = vec!["close"];
    let first_ids_vec: Vec<String> = first_ids.iter().cloned().collect();
    for id in &first_ids_vec {
        close_args.push(id.as_str());
    }
    run_br(dir.path(), &close_args);

    let second_events = adapter.poll().await.unwrap();
    let second_ids = event_ids(&second_events);
    assert_eq!(
        second_ids, unfetched_ids,
        "older rows outside the first saturated batch must remain observable"
    );
    assert!(
        second_events
            .iter()
            .all(|event| matches!(event, PmEvent::IssueCreated(_))),
        "had_prior must stay false while the cursor remains unset"
    );
}
