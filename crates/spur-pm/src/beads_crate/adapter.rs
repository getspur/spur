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
use std::time::Duration;

use crate::beads_crate::backoff::BackoffPolicy;
use crate::beads_crate::init;
use crate::beads_crate::metrics::ContentionMetrics;

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
}
