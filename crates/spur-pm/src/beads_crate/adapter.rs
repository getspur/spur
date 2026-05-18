//! `BeadsCrateAdapter` — direct-linkage adapter to `beads_rust` 0.2.1.
//!
//! Open-fresh-per-call shape: the `SqliteStorage` handle is never shared
//! across async boundaries. We mirror beads_rust's own MCP module — store only
//! paths and config in the adapter, open a new `SqliteStorage` inside each
//! `spawn_blocking` invocation, and drop it when the closure returns. The
//! closure result must be `Send`, but the storage itself never crosses thread
//! boundaries.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::beads_crate::backoff::BackoffPolicy;
use crate::beads_crate::init;
use crate::beads_crate::metrics::ContentionMetrics;
use crate::beads_crate::snapshot::{Conflict, Snapshot};
use crate::beads_crate::{wal_checkpoint, write_lock};
use crate::poll_cursor::PollCursor;

/// Coarse data_version proxy. beads_rust 0.2.1 does not expose
/// `PRAGMA data_version`; until it does, we use `count_issues()`. This
/// detects net add/delete between snapshot and commit, which covers
/// the IssueTracker CAS use cases (e.g. "delete iff still present").
/// It MISSES pure field updates that don't change the row count —
/// callers who need that level of strictness must not rely on this
/// proxy yet. Follow-up: expose PRAGMA data_version upstream or
/// vendor a helper.
fn read_data_version(s: &beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<i64> {
    let count = s.count_issues()?;
    Ok(count as i64)
}

#[allow(dead_code)]
fn acquire_write_lock_with_backoff(
    beads_dir: &Path,
    backoff: &BackoffPolicy,
    lock_timeout_ms: u64,
    metrics: &ContentionMetrics,
) -> anyhow::Result<std::fs::File> {
    let start = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        let attempt_start = Instant::now();
        match write_lock::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms)) {
            Ok(file) => {
                metrics.record_lock_wait(attempt_start.elapsed());
                return Ok(file);
            }
            Err(_) => {
                metrics.incr_busy();
                let elapsed = start.elapsed();
                let rand = fastrand_unit();
                let Some(delay) = backoff.step(attempt, elapsed, rand) else {
                    metrics.incr_ceiling();
                    anyhow::bail!(
                        "write lock acquisition exceeded ceiling after {:?}",
                        elapsed
                    );
                };
                std::thread::sleep(delay);
                attempt += 1;
            }
        }
    }
}

async fn acquire_write_lock_async(
    beads_dir: PathBuf,
    backoff: BackoffPolicy,
    lock_timeout_ms: u64,
    metrics: Arc<ContentionMetrics>,
) -> anyhow::Result<std::fs::File> {
    let start = Instant::now();
    let timeout = Duration::from_millis(lock_timeout_ms);
    let mut attempt: u32 = 0;

    loop {
        let dir_for_attempt = beads_dir.clone();
        match tokio::task::spawn_blocking(move || {
            write_lock::try_blocking_write_lock_once(&dir_for_attempt)
        })
        .await
        .map_err(|e| anyhow::anyhow!("write lock attempt task failed: {e}"))??
        {
            write_lock::WriteLockAttempt::Acquired(file) => {
                metrics.record_lock_wait(start.elapsed());
                return Ok(file);
            }
            write_lock::WriteLockAttempt::Busy => {
                metrics.incr_busy();
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    metrics.incr_ceiling();
                    anyhow::bail!("write lock acquisition timed out after {lock_timeout_ms}ms");
                }
                let rand = fastrand_unit();
                let Some(delay) = backoff.step(attempt, elapsed, rand) else {
                    metrics.incr_ceiling();
                    anyhow::bail!(
                        "write lock acquisition exceeded ceiling after {:?}",
                        elapsed
                    );
                };
                let remaining = timeout.saturating_sub(start.elapsed());
                tokio::time::sleep(
                    delay
                        .min(write_lock::WRITE_LOCK_POLL_INTERVAL)
                        .min(remaining),
                )
                .await;
                attempt += 1;
            }
        }
    }
}

fn fastrand_unit() -> f64 {
    use std::cell::Cell;
    thread_local! { static SEED: Cell<u64> = const { Cell::new(0x9E3779B97F4A7C15) }; }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        ((x as u32) as f64) / (u32::MAX as f64)
    })
}

#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub lock_timeout_ms: u64,
    pub stale_tmp_min_age: Duration,
    pub allow_non_local_fs: bool,
    pub backoff: BackoffPolicy,
    pub actor: String,
    /// Optional poll cursor file. When set, `open()` loads the cursor from
    /// this path if it exists, and every successful `poll()` writes the latest
    /// cursor back to the same path.
    pub cursor_path: Option<PathBuf>,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            lock_timeout_ms: 5_000,
            stale_tmp_min_age: Duration::from_secs(3600),
            allow_non_local_fs: false,
            backoff: BackoffPolicy::default(),
            actor: "spur".to_string(),
            cursor_path: None,
        }
    }
}

#[allow(dead_code)] // jsonl_path and config wired by T11–T14
pub struct BeadsCrateAdapter {
    pub(crate) beads_dir: PathBuf,
    pub(crate) jsonl_path: PathBuf,
    pub(crate) config: AdapterConfig,
    pub(crate) metrics: Arc<ContentionMetrics>,
    /// Boundary-safe poll cursor; `None` until the first `poll()` call so a
    /// fresh adapter emits all open issues as `IssueCreated` on first poll
    /// (matching `BeadsAdapter` semantics in `beads.rs`).
    pub(crate) cursor: tokio::sync::Mutex<Option<PollCursor>>,
}

impl BeadsCrateAdapter {
    pub async fn open(beads_dir: &Path, config: AdapterConfig) -> anyhow::Result<Self> {
        let beads_dir = beads_dir.to_path_buf();
        let jsonl_path = beads_dir.join("issues.jsonl");

        let dir_for_init = beads_dir.clone();
        let cfg_for_init = config.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if !cfg_for_init.allow_non_local_fs {
                init::detect_local_fs(&dir_for_init)?;
            }
            init::init_writer_with_flush(
                &dir_for_init,
                cfg_for_init.lock_timeout_ms,
                cfg_for_init.stale_tmp_min_age,
            )?;
            Ok(())
        })
        .await??;

        let metrics = Arc::new(ContentionMetrics::default());
        let initial_cursor = match config.cursor_path.as_ref() {
            Some(path) => match PollCursor::load_from(path) {
                Ok(cursor) => cursor,
                Err(e) => {
                    tracing::warn!(
                        ?path,
                        "failed to load cursor file ({e}); starting without cursor"
                    );
                    None
                }
            },
            None => None,
        };

        Ok(Self {
            beads_dir,
            jsonl_path,
            config,
            metrics,
            cursor: tokio::sync::Mutex::new(initial_cursor),
        })
    }

    pub fn metrics(&self) -> &ContentionMetrics {
        &self.metrics
    }

    /// Idempotent under-lock auto-flush. Inside `.beads/.write.lock`, calls
    /// `beads_rust::sync::auto_flush` which is a no-op if nothing is dirty.
    /// Safe to call concurrently across processes — they serialize on the flock.
    ///
    /// Must NOT be called from inside a `write()` or `batch()` closure: the
    /// underlying file flock is not reentrant within the same process and
    /// the second acquisition would deadlock.
    pub async fn auto_flush(&self) -> anyhow::Result<()> {
        let beads_dir = self.beads_dir.clone();
        let backoff = self.config.backoff.clone();
        let lock_timeout_ms = self.config.lock_timeout_ms;
        let metrics = Arc::clone(&self.metrics);
        let flock = acquire_write_lock_async(
            beads_dir.clone(),
            backoff,
            lock_timeout_ms,
            Arc::clone(&metrics),
        )
        .await?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let _flock = flock;
            let db_path = beads_dir.join("beads.db");
            let mut storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &db_path,
                Some(lock_timeout_ms),
            )?;
            let outcome = beads_rust::sync::auto_flush(&mut storage, &beads_dir);
            drop(storage);
            wal_checkpoint::checkpoint_wal_truncate_best_effort(&db_path);
            let outcome = outcome?;
            if outcome.flushed {
                metrics.incr_auto_flush_dirty();
                metrics.incr_auto_flush_success();
            } else {
                metrics.incr_auto_flush_skipped();
            }
            Ok(())
        })
        .await?
    }

    pub(crate) fn actor(&self) -> String {
        self.config.actor.clone()
    }

    /// Lock-free snapshot read. Opens a fresh `SqliteStorage` connection
    /// for the duration of `f` and drops it on return. WAL mode gives
    /// snapshot isolation across concurrent readers and writers.
    pub async fn read<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<T>
            + Send
            + 'static,
        T: Send + 'static,
    {
        let metrics = Arc::clone(&self.metrics);
        let db_path = self.beads_dir.join("beads.db");
        let lock_timeout_ms = self.config.lock_timeout_ms;
        // PROBE: issue_detail_latency
        let dispatch_started = Instant::now();
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            // PROBE: issue_detail_latency — measure spawn_blocking queue wait,
            // sqlite Connection::open + pragmas, and the read closure body.
            let blocking_entered = Instant::now();
            let spawn_blocking_queue_ms = blocking_entered
                .duration_since(dispatch_started)
                .as_millis() as u64;
            metrics.incr_read();
            let open_started = Instant::now();
            let storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &db_path,
                Some(lock_timeout_ms),
            )?;
            let sqlite_open_ms = open_started.elapsed().as_millis() as u64;
            let closure_started = Instant::now();
            let result = f(&storage);
            let closure_ms = closure_started.elapsed().as_millis() as u64;
            tracing::info!(
                target: "issue_probe",
                site = "beads_read",
                spawn_blocking_queue_ms = spawn_blocking_queue_ms,
                sqlite_open_ms = sqlite_open_ms,
                closure_ms = closure_ms,
                total_ms = dispatch_started.elapsed().as_millis() as u64,
                "BeadsCrateAdapter::read timing",
            );
            result
        })
        .await?
    }

    /// Read a snapshot value plus the current data_version proxy so the
    /// caller can do async work and then call `validate_and_commit`. The
    /// snapshot is consistent at read time; if the db changes before
    /// `validate_and_commit`, the commit returns a `Conflict` error.
    pub async fn read_snapshot<S, F>(&self, f: F) -> anyhow::Result<Snapshot<S>>
    where
        F: FnOnce(&beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<S>
            + Send
            + 'static,
        S: Send + 'static,
    {
        let metrics = Arc::clone(&self.metrics);
        let db_path = self.beads_dir.join("beads.db");
        let lock_timeout_ms = self.config.lock_timeout_ms;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Snapshot<S>> {
            metrics.incr_read();
            let storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &db_path,
                Some(lock_timeout_ms),
            )?;
            let value = f(&storage)?;
            let data_version = read_data_version(&storage)?;
            Ok(Snapshot {
                value,
                data_version,
            })
        })
        .await?
    }

    /// Apply a write conditioned on the snapshot's data_version still
    /// matching at commit time. Returns a `Conflict` error if the
    /// underlying state moved between read and validate. The closure
    /// receives the snapshot's value alongside `&mut SqliteStorage`.
    pub async fn validate_and_commit<S, T, FW>(
        &self,
        snapshot: Snapshot<S>,
        write: FW,
    ) -> anyhow::Result<T>
    where
        FW: FnOnce(&mut beads_rust::storage::sqlite::SqliteStorage, S) -> anyhow::Result<T>
            + Send
            + 'static,
        S: Send + 'static,
        T: Send + 'static,
    {
        let metrics = Arc::clone(&self.metrics);
        let beads_dir = self.beads_dir.clone();
        let backoff = self.config.backoff.clone();
        let lock_timeout_ms = self.config.lock_timeout_ms;
        let flock = acquire_write_lock_async(
            beads_dir.clone(),
            backoff,
            lock_timeout_ms,
            Arc::clone(&metrics),
        )
        .await?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            let _flock = flock;
            let db_path = beads_dir.join("beads.db");
            let mut storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &db_path,
                Some(lock_timeout_ms),
            )?;
            let current = read_data_version(&storage)?;
            if current != snapshot.data_version {
                metrics.incr_conflict();
                anyhow::bail!(Conflict::data_version(snapshot.data_version, current));
            }
            metrics.incr_write();
            let result = write(&mut storage, snapshot.value);
            if result.is_err() {
                metrics.incr_write_error();
            }
            drop(storage);
            wal_checkpoint::checkpoint_wal_truncate_best_effort(&db_path);
            result
        })
        .await?
    }

    /// Multiple mutations under one flock acquisition. NOT a single DB
    /// transaction — each individual call inside `f` is atomic, but the
    /// batch as a whole is not. See spec "Multi-statement atomicity"
    /// non-goal.
    pub async fn batch<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&mut beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<T>
            + Send
            + 'static,
        T: Send + 'static,
    {
        // Same shape as `write`; the API distinction signals intent.
        self.write(f).await
    }

    /// Single write under cross-process flock. Opens a fresh
    /// `SqliteStorage` AFTER acquiring `.write.lock`, runs the closure,
    /// drops both. Each `beads_rust` mutation method is internally
    /// atomic via the crate's `with_write_transaction`.
    pub async fn write<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&mut beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<T>
            + Send
            + 'static,
        T: Send + 'static,
    {
        let metrics = Arc::clone(&self.metrics);
        let beads_dir = self.beads_dir.clone();
        let backoff = self.config.backoff.clone();
        let lock_timeout_ms = self.config.lock_timeout_ms;
        let flock = acquire_write_lock_async(
            beads_dir.clone(),
            backoff,
            lock_timeout_ms,
            Arc::clone(&metrics),
        )
        .await?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            let _flock = flock;
            let db_path = beads_dir.join("beads.db");
            let mut storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &db_path,
                Some(lock_timeout_ms),
            )?;
            metrics.incr_write();
            let result = f(&mut storage);
            if result.is_err() {
                metrics.incr_write_error();
            }
            drop(storage);
            wal_checkpoint::checkpoint_wal_truncate_best_effort(&db_path);
            result
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::adapter::IssueTracker;
    use crate::types::IssueCreate;

    #[tokio::test(flavor = "multi_thread")]
    async fn open_succeeds_in_fresh_dir() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        assert!(adapter.beads_dir.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_persists_across_open() {
        let dir = TempDir::new().unwrap();
        let cursor_path = dir.path().join(".spur-test-cursor");
        let config = AdapterConfig {
            cursor_path: Some(cursor_path.clone()),
            ..AdapterConfig::default()
        };

        let first_cursor = {
            let adapter = BeadsCrateAdapter::open(dir.path(), config.clone())
                .await
                .unwrap();
            adapter
                .create_issue(IssueCreate {
                    title: "persisted cursor issue".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            let events = adapter.poll().await.unwrap();
            assert_eq!(events.len(), 1);
            assert!(cursor_path.exists(), "poll should write cursor file");

            let cursor = {
                let guard = adapter.cursor.lock().await;
                guard.clone()
            };
            cursor.expect("poll should set cursor")
        };

        let reopened = BeadsCrateAdapter::open(dir.path(), config).await.unwrap();
        let loaded_cursor = reopened
            .cursor
            .lock()
            .await
            .clone()
            .expect("open should load cursor from disk");

        assert_eq!(loaded_cursor.ts, first_cursor.ts);
        assert_eq!(loaded_cursor.ids_at_boundary, first_cursor.ids_at_boundary);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_path_set_but_file_missing_starts_empty() {
        let dir = TempDir::new().unwrap();
        let cursor_path = dir.path().join(".spur-cursor-does-not-exist");
        assert!(!cursor_path.exists(), "test precondition");

        let adapter = BeadsCrateAdapter::open(
            dir.path(),
            AdapterConfig {
                cursor_path: Some(cursor_path),
                ..AdapterConfig::default()
            },
        )
        .await
        .expect("open should succeed when cursor_path is set but file is absent");

        assert!(
            adapter.cursor.lock().await.is_none(),
            "missing cursor file must produce a None cursor, not panic or error"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_returns_count_for_empty_db() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        let count = adapter
            .read(|s| Ok(s.count_issues()?))
            .await
            .expect("read closure runs");
        assert_eq!(count, 0);
        assert_eq!(
            adapter
                .metrics()
                .read_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_runs_closure_under_flock() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        let result: i32 = adapter.write(|_s| Ok(42)).await.unwrap();
        assert_eq!(result, 42);
        assert_eq!(
            adapter
                .metrics()
                .write_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_flush_idempotent_when_clean() {
        use std::sync::atomic::Ordering;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        adapter.auto_flush().await.unwrap();
        let success_after_first = adapter
            .metrics()
            .auto_flush_success_total
            .load(Ordering::Relaxed);
        let skipped_after_first = adapter
            .metrics()
            .auto_flush_skipped_total
            .load(Ordering::Relaxed);

        adapter.auto_flush().await.unwrap();
        let success_after_second = adapter
            .metrics()
            .auto_flush_success_total
            .load(Ordering::Relaxed);
        let skipped_after_second = adapter
            .metrics()
            .auto_flush_skipped_total
            .load(Ordering::Relaxed);

        assert_eq!(
            success_after_second, success_after_first,
            "clean re-flush must NOT bump success counter (would inflate telemetry)"
        );
        assert_eq!(
            skipped_after_second - skipped_after_first,
            1,
            "clean re-flush must bump skipped counter exactly once"
        );
    }

    /// Exercises the CAS rejection path. The data_version proxy is
    /// `count_issues()`; we simulate a concurrent net-add by running an
    /// `INSERT INTO issues` directly via the writer between snapshot
    /// and commit, then verify validate_and_commit aborts with a
    /// Conflict.
    #[tokio::test(flavor = "multi_thread")]
    async fn validate_and_commit_rejects_on_data_version_drift() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let snap = adapter.read_snapshot(|_s| Ok(())).await.unwrap();
        // Simulate a concurrent writer bumping the proxy. Construct a
        // minimal Issue via struct literal — Issue derives no Default.
        adapter
            .write(|s| {
                use beads_rust::model::{Issue, IssueType, Priority, Status};
                use chrono::Utc;
                let now = Utc::now();
                let issue = Issue {
                    id: "bd-test-cas".into(),
                    title: "drift".into(),
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
                };
                s.create_issue(&issue, "test").map_err(Into::into)
            })
            .await
            .unwrap();

        let err = adapter
            .validate_and_commit(snap, |_s, _| Ok::<i32, anyhow::Error>(99))
            .await
            .expect_err("commit must reject after drift");
        let msg = err.to_string();
        assert!(
            msg.contains("snapshot CAS conflict"),
            "expected Conflict, got: {msg}"
        );
        assert_eq!(
            adapter
                .metrics()
                .conflict_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_snapshot_then_validate_and_commit_no_conflict() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        // Snapshot the empty db (count_issues == 0).
        let snap = adapter.read_snapshot(|_s| Ok(())).await.unwrap();
        // Validate-and-commit without any concurrent writers — should
        // succeed because data_version proxy hasn't moved.
        let result: i32 = adapter
            .validate_and_commit(snap, |_s, _| Ok(7))
            .await
            .unwrap();
        assert_eq!(result, 7);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_runs_multiple_steps_under_one_lock() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        let count = adapter
            .batch(|_s| {
                let mut acc = 0_usize;
                for _ in 0..5 {
                    acc += 1;
                }
                Ok(acc)
            })
            .await
            .unwrap();
        assert_eq!(count, 5);
        // Single batch = single write op recorded
        assert_eq!(
            adapter
                .metrics()
                .write_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn read_completes_while_writer_waits_for_flock_on_single_blocking_thread() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let dir = TempDir::new().unwrap();
            let adapter = Arc::new(
                BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
                    .await
                    .unwrap(),
            );
            let held_lock =
                write_lock::blocking_write_lock_with_timeout(dir.path(), Some(50)).unwrap();

            let writer = {
                let adapter = Arc::clone(&adapter);
                tokio::spawn(async move { adapter.write(|_s| Ok::<_, anyhow::Error>(())).await })
            };

            while adapter
                .metrics()
                .lock_busy_total
                .load(std::sync::atomic::Ordering::Relaxed)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let read = tokio::time::timeout(Duration::from_millis(100), async {
                adapter.read(|s| Ok(s.count_issues()?)).await
            })
            .await;

            drop(held_lock);
            writer.await.unwrap().unwrap();

            assert!(
                read.is_ok(),
                "read should not queue behind an async writer waiting for the flock"
            );
            assert_eq!(read.unwrap().unwrap(), 0);
        });
    }
}
