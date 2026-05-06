//! Multi-process integration tests for `BeadsCrateAdapter`.
//!
//! Each test simulates multiple "SPUR instances" against the same
//! `.beads/` directory by opening multiple adapters in the same process.
//! The cross-process flock at `.beads/.write.lock` serializes mutations
//! identically whether contention is intra- or inter-process, so this
//! is a faithful proxy for true multi-process behavior without the
//! overhead of spawning child processes.

use beads_rust::model::{Issue, IssueType, Priority, Status};
use beads_rust::storage::sqlite::ListFilters;
use chrono::Utc;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use tempfile::TempDir;

fn make_issue(id: impl Into<String>, title: impl Into<String>) -> Issue {
    let now = Utc::now();
    Issue {
        id: id.into(),
        title: title.into(),
        description: None,
        status: Status::Open,
        priority: Priority::MEDIUM,
        issue_type: IssueType::Task,
        created_at: now,
        updated_at: now,
        assignee: None,
        owner: None,
        estimated_minutes: None,
        due_at: None,
        defer_until: None,
        external_ref: None,
        ephemeral: false,
        content_hash: None,
        design: None,
        acceptance_criteria: None,
        notes: None,
        created_by: None,
        closed_at: None,
        close_reason: None,
        closed_by_session: None,
        source_system: None,
        source_repo: None,
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        original_type: None,
        compaction_level: None,
        compacted_at: None,
        compacted_at_commit: None,
        original_size: None,
        sender: None,
        pinned: false,
        is_template: false,
        labels: Vec::new(),
        dependencies: Vec::new(),
        comments: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writes_no_corruption() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    // Each open() and each write() takes .write.lock; with 4 contenders
    // doing 25 writes apiece the queue depth swamps the 5s default.
    // 30s is comfortable headroom on a CI box without making a hang
    // hide indefinitely.
    let cfg = || AdapterConfig {
        lock_timeout_ms: 30_000,
        ..AdapterConfig::default()
    };

    // 4 simulated instances × 25 writes each = 100 issues. The
    // cross-process .write.lock serializes them.
    let mut handles = Vec::new();
    for instance in 0..4 {
        let path = path.clone();
        handles.push(tokio::spawn(async move {
            let adapter = BeadsCrateAdapter::open(&path, cfg())
                .await
                .expect("adapter opens");
            for i in 0..25 {
                let id = format!("bd-mp-{instance}-{i}");
                let title = format!("inst{instance} #{i}");
                adapter
                    .write(move |s| {
                        s.create_issue(&make_issue(id, title), "test")
                            .map_err(anyhow::Error::from)
                    })
                    .await
                    .expect("write succeeds");
            }
        }));
    }
    for h in handles {
        h.await.expect("task joins cleanly");
    }

    let adapter = BeadsCrateAdapter::open(&path, cfg())
        .await
        .expect("adapter reopens");
    let issues = adapter
        .read(|s| s.list_issues(&ListFilters::default()).map_err(Into::into))
        .await
        .expect("list_issues succeeds");
    assert_eq!(
        issues.len(),
        100,
        "all 100 writes survived across simulated instances"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_open_serializes_via_migration_lock() {
    let dir = TempDir::new().unwrap();
    let p1 = dir.path().to_path_buf();
    let p2 = dir.path().to_path_buf();

    let h1 =
        tokio::spawn(async move { BeadsCrateAdapter::open(&p1, AdapterConfig::default()).await });
    let h2 =
        tokio::spawn(async move { BeadsCrateAdapter::open(&p2, AdapterConfig::default()).await });

    let r1 = h1.await.expect("task1 joins");
    let r2 = h2.await.expect("task2 joins");
    assert!(r1.is_ok(), "first open succeeded: {:?}", r1.err());
    assert!(
        r2.is_ok(),
        "second open succeeded after migration: {:?}",
        r2.err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_conflict_detected() {
    let dir = TempDir::new().unwrap();
    let adapter1 = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
        .await
        .expect("adapter1 opens");
    let adapter2 = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
        .await
        .expect("adapter2 opens");

    // adapter1 seeds an issue and then snapshots state.
    adapter1
        .write(|s| {
            s.create_issue(&make_issue("bd-snap-seed", "seed"), "test")
                .map_err(anyhow::Error::from)
        })
        .await
        .expect("seed write");
    let snap = adapter1
        .read_snapshot(|_s| Ok(()))
        .await
        .expect("snapshot taken");

    // adapter2 mutates the workspace between snapshot and commit. The
    // current data_version proxy is `count_issues()`
    // (see adapter::read_data_version), so we drive a net-add to bump
    // it; a pure status update would leave the row count unchanged and
    // slip past the proxy.
    adapter2
        .write(|s| {
            s.create_issue(&make_issue("bd-snap-other", "other"), "test")
                .map_err(anyhow::Error::from)
        })
        .await
        .expect("intervening write");

    let result = adapter1
        .validate_and_commit(snap, |s, _| {
            s.create_issue(&make_issue("bd-snap-after", "after"), "test")
                .map_err(anyhow::Error::from)
        })
        .await;

    assert!(result.is_err(), "expected Conflict, got {result:?}");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("snapshot CAS conflict"),
        "expected Conflict message, got: {err_msg}"
    );
}
