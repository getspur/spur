//! `BeadsCrateAdapter` — direct-linkage adapter to `beads_rust` 0.2.1.
//!
//! Open-fresh-per-call shape: `SqliteStorage` is `!Send` (fsqlite uses
//! `Rc<RefCell<…>>`), so we mirror beads_rust's own MCP module —
//! store only paths and config in the adapter, open a new `SqliteStorage`
//! inside each `spawn_blocking` invocation, and drop it when the closure
//! returns. The closure result must be `Send`, but the storage itself
//! never crosses thread boundaries.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use beads_rust::sync;

use crate::beads_crate::backoff::BackoffPolicy;
use crate::beads_crate::init;
use crate::beads_crate::metrics::ContentionMetrics;

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
        match sync::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms)) {
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
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            lock_timeout_ms: 5_000,
            stale_tmp_min_age: Duration::from_secs(3600),
            allow_non_local_fs: false,
            backoff: BackoffPolicy::default(),
        }
    }
}

#[allow(dead_code)] // jsonl_path and config wired by T11–T14
pub struct BeadsCrateAdapter {
    pub(crate) beads_dir: PathBuf,
    pub(crate) jsonl_path: PathBuf,
    pub(crate) config: AdapterConfig,
    pub(crate) metrics: Arc<ContentionMetrics>,
}

impl BeadsCrateAdapter {
    pub async fn open(beads_dir: &Path, config: AdapterConfig) -> anyhow::Result<Self> {
        let beads_dir = beads_dir.to_path_buf();
        let jsonl_path = beads_dir.join("issues.jsonl");

        let dir_for_init = beads_dir.clone();
        let jsonl_for_init = jsonl_path.clone();
        let cfg_for_init = config.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if !cfg_for_init.allow_non_local_fs {
                init::detect_local_fs(&dir_for_init)?;
            }
            let mut writer = init::open_writer_under_migration_lock(
                &dir_for_init,
                cfg_for_init.lock_timeout_ms,
            )?;
            let _ = init::sweep_stale_jsonl_temps(&dir_for_init, cfg_for_init.stale_tmp_min_age);
            let _ = init::detect_and_force_flush_stale_jsonl(
                &mut writer,
                &dir_for_init,
                &jsonl_for_init,
            );
            Ok(())
        })
        .await??;

        let metrics = Arc::new(ContentionMetrics::default());

        Ok(Self {
            beads_dir,
            jsonl_path,
            config,
            metrics,
        })
    }

    pub fn metrics(&self) -> &ContentionMetrics {
        &self.metrics
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
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            metrics.incr_read();
            let storage = beads_rust::storage::sqlite::SqliteStorage::open(&db_path)?;
            f(&storage)
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
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            let _flock =
                acquire_write_lock_with_backoff(&beads_dir, &backoff, lock_timeout_ms, &metrics)?;
            let mut storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
                &beads_dir.join("beads.db"),
                Some(lock_timeout_ms),
            )?;
            metrics.incr_write();
            let result = f(&mut storage);
            if result.is_err() {
                metrics.incr_write_error();
            }
            result
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test(flavor = "multi_thread")]
    async fn open_succeeds_in_fresh_dir() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        assert!(adapter.beads_dir.exists());
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
}
