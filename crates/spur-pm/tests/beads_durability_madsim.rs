#![cfg(madsim)]
#![allow(unexpected_cfgs)]

extern crate madsim_tokio as tokio;

use std::path::Path;
use std::time::{Duration, SystemTime};

use beads_rust::model::{Issue, IssueType, Priority, Status};
use beads_rust::storage::sqlite::{ListFilters, SqliteStorage};
use chrono::Utc;
use filetime::{set_file_mtime, FileTime};
use spur_pm::beads_crate::test_helpers;
use tokio::sync::oneshot;

#[test]
fn lock_holder_crash_releases_flock_and_next_writer_overwrites_payload() {
    allow_system_threads_for_lock_heartbeat();
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();
        let dir = tempfile::TempDir::new().unwrap();
        let beads_dir = dir.path().to_path_buf();
        let lock_path = beads_dir.join(".write.lock");
        let (acquired_tx, acquired_rx) = oneshot::channel();

        let writer = {
            let beads_dir = beads_dir.clone();
            tokio::spawn(async move {
                let _guard =
                    test_helpers::blocking_write_lock_with_timeout(&beads_dir, Some(100)).unwrap();
                acquired_tx.send(()).unwrap();
                tokio::time::sleep(Duration::from_secs(60)).await;
            })
        };

        acquired_rx.await.unwrap();
        let stale_payload = std::fs::read_to_string(&lock_path).unwrap();
        writer.abort();
        let _ = writer.await;
        sleep_with_shared_timeout(Duration::from_millis(2)).await;

        let guard = test_helpers::blocking_write_lock_with_timeout(&beads_dir, Some(100)).unwrap();
        let next_payload = std::fs::read_to_string(&lock_path).unwrap();
        assert!(
            lock_path.exists(),
            "lock file remains after cancelled holder"
        );
        assert_ne!(
            stale_payload, next_payload,
            "new holder should rewrite stale lock payload"
        );
        assert_eq!(
            test_helpers::lock_holder_pid(&lock_path),
            Some(std::process::id())
        );

        create_issue(&beads_dir, "bd-madsim-crash");
        assert_no_jsonl_temps(&beads_dir);
        assert_quick_check(&beads_dir.join("beads.db"));
        drop(guard);
    });
}

#[test]
fn wal_checkpoint_busy_preserves_committed_row_without_frankenwal_artifact() {
    allow_system_threads_for_lock_heartbeat();
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();
        let dir = tempfile::TempDir::new().unwrap();
        let beads_dir = dir.path().to_path_buf();
        let db_path = beads_dir.join("beads.db");

        create_issue(&beads_dir, "bd-madsim-seed");
        let reader = rusqlite::Connection::open(&db_path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
            .unwrap();

        create_issue(&beads_dir, "bd-madsim-checkpoint-busy");
        test_helpers::checkpoint_wal_truncate_best_effort(&db_path);
        drop(reader);

        assert_quick_check(&db_path);
        assert_issue_visible(&beads_dir, "bd-madsim-checkpoint-busy");
        assert!(
            !top_level_names(&beads_dir)
                .iter()
                .any(|name| name.contains("frankenwal")),
            "busy checkpoint must not create frankenwal artifacts"
        );
    });
}

#[test]
fn torn_jsonl_temp_sweep_only_removes_old_temps_and_keeps_generations_matched() {
    allow_system_threads_for_lock_heartbeat();
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_from_env();
        let dir = tempfile::TempDir::new().unwrap();
        let beads_dir = dir.path().to_path_buf();

        create_issue(&beads_dir, "bd-madsim-jsonl-seed");
        auto_flush(&beads_dir);

        let old_tmp = beads_dir.join("issues.jsonl.111.tmp");
        let recent_tmp = beads_dir.join("issues.jsonl.222.tmp");
        std::fs::write(&old_tmp, b"{\"torn\":").unwrap();
        std::fs::write(&recent_tmp, b"{\"recent\":").unwrap();
        set_file_mtime(
            &old_tmp,
            FileTime::from_system_time(SystemTime::now() - Duration::from_secs(7_200)),
        )
        .unwrap();

        let _guard = test_helpers::blocking_write_lock_with_timeout(&beads_dir, Some(100)).unwrap();
        let removed =
            test_helpers::sweep_stale_jsonl_temps(&beads_dir, Duration::from_secs(3_600)).unwrap();
        assert_eq!(removed, 1);
        assert!(!old_tmp.exists());
        assert!(recent_tmp.exists());
        drop(_guard);

        create_issue(&beads_dir, "bd-madsim-jsonl-after");
        auto_flush(&beads_dir);

        assert_jsonl_parses(&beads_dir.join("issues.jsonl"));
        assert_eq!(db_issue_count(&beads_dir), jsonl_issue_count(&beads_dir));
        assert!(
            !top_level_names(&beads_dir)
                .iter()
                .any(|name| name.contains("broken")),
            "torn temp sweep must not leave broken JSONL artifacts"
        );
    });
}

fn make_issue(id: &str) -> Issue {
    let now = Utc::now();
    Issue {
        id: id.into(),
        title: id.into(),
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

fn create_issue(beads_dir: &Path, id: &str) {
    let db_path = beads_dir.join("beads.db");
    let mut storage = SqliteStorage::open_with_timeout(&db_path, Some(5_000)).unwrap();
    storage
        .create_issue(&make_issue(id), "madsim")
        .expect("issue write succeeds");
}

fn auto_flush(beads_dir: &Path) {
    let db_path = beads_dir.join("beads.db");
    let mut storage = SqliteStorage::open_with_timeout(&db_path, Some(5_000)).unwrap();
    beads_rust::sync::auto_flush(&mut storage, beads_dir).expect("auto_flush succeeds");
}

fn assert_issue_visible(beads_dir: &Path, id: &str) {
    let db_path = beads_dir.join("beads.db");
    let storage = SqliteStorage::open_with_timeout(&db_path, Some(5_000)).unwrap();
    let issues = storage.list_issues(&ListFilters::default()).unwrap();
    assert!(issues.iter().any(|issue| issue.id == id), "{id} is visible");
}

fn db_issue_count(beads_dir: &Path) -> usize {
    let db_path = beads_dir.join("beads.db");
    let storage = SqliteStorage::open_with_timeout(&db_path, Some(5_000)).unwrap();
    storage.list_issues(&ListFilters::default()).unwrap().len()
}

fn jsonl_issue_count(beads_dir: &Path) -> usize {
    std::fs::read_to_string(beads_dir.join("issues.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn assert_jsonl_parses(path: &Path) {
    for line in std::fs::read_to_string(path).unwrap().lines() {
        if !line.trim().is_empty() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }
}

fn assert_quick_check(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let result: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(result, "ok");
}

fn assert_no_jsonl_temps(beads_dir: &Path) {
    assert!(
        !top_level_names(beads_dir)
            .iter()
            .any(|name| name.starts_with("issues.jsonl.") && name.ends_with(".tmp")),
        "no temp JSONL files should remain"
    );
}

fn top_level_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

async fn sleep_with_shared_timeout(duration: Duration) {
    let deadline = tokio::time::Instant::now() + duration + Duration::from_millis(1);
    spur_test_madsim::timeout_at(deadline, tokio::time::sleep(duration))
        .await
        .expect("shared madsim timeout helper should not elapse");
}

fn assert_seed_from_env() {
    if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
        assert_eq!(
            madsim::runtime::Handle::current().seed(),
            seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
        );
    }
}

fn allow_system_threads_for_lock_heartbeat() {
    std::env::set_var("MADSIM_ALLOW_SYSTEM_THREAD", "1");
}
