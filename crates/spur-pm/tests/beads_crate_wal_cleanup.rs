use std::fs;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use beads_rust::model::{Issue, IssueType, Priority, Status};
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
async fn write_requests_actor_checkpoint() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        let checkpoints_before = checkpoint_total(&adapter);

        adapter
            .write(|s| {
                s.create_issue(&make_issue("bd-wal-cleanup", "WAL cleanup"), "test")
                    .map_err(anyhow::Error::from)
            })
            .await
            .expect("write succeeds");

        wait_for_checkpoint_after(&adapter, checkpoints_before).await;
    })
    .await
    .expect("writer actor should service post-write checkpoint request before the deadline");
}

#[tokio::test(flavor = "multi_thread")]
async fn wal_sidecar_stays_bounded_after_write_burst_and_checkpoint() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        let checkpoints_before = checkpoint_total(&adapter);

        for i in 0..64 {
            let id = format!("bd-wal-burst-{i}");
            let title = format!("WAL burst {i}");
            adapter
                .write(move |s| {
                    s.create_issue(&make_issue(id, title), "test")
                        .map_err(anyhow::Error::from)
                })
                .await
                .expect("write succeeds");
        }

        wait_for_checkpoint_after(&adapter, checkpoints_before).await;

        let wal_path = dir.path().join("beads.db-wal");
        let wal_len = fs::metadata(&wal_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let max_bounded_wal_bytes = 128 * 1024;
        assert!(
            wal_len <= max_bounded_wal_bytes,
            "WAL sidecar should stay bounded after checkpoints: {} bytes > {} bytes",
            wal_len,
            max_bounded_wal_bytes
        );
    })
    .await
    .expect("WAL burst and checkpoint should complete before the deadline");
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_adapter_mid_load_joins_under_one_second() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (_release_tx, release_rx) = mpsc::sync_channel::<()>(1);

        let mut write = Box::pin(adapter.write(move |_s| {
            entered_tx.send(()).expect("test receiver is alive");
            let _ = release_rx.recv_timeout(Duration::from_millis(150));
            Ok::<_, anyhow::Error>(())
        }));
        let entered =
            tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(1)));

        tokio::select! {
            result = &mut write => panic!("write completed before drop setup: {result:?}"),
            entered = entered => entered
                .expect("entered wait task joins")
                .expect("writer job should start before the deadline"),
        }

        drop(write);
        let drop_started = Instant::now();
        drop(adapter);
        let drop_elapsed = drop_started.elapsed();

        assert!(
            drop_elapsed < Duration::from_secs(1),
            "adapter drop took {drop_elapsed:?}, expected < 1s"
        );
    })
    .await
    .expect("drop-under-load test should complete before the deadline");
}

fn checkpoint_total(adapter: &BeadsCrateAdapter) -> u64 {
    adapter
        .metrics()
        .checkpoint_total
        .load(std::sync::atomic::Ordering::Relaxed)
}

async fn wait_for_checkpoint_after(adapter: &BeadsCrateAdapter, checkpoints_before: u64) {
    loop {
        if checkpoint_total(adapter) > checkpoints_before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
