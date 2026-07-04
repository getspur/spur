use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use anyhow::{Context as _, Result};
use tokio::sync::{Mutex, OnceCell};

pub(crate) const DELTA_FAILURE_ESCALATION_THRESHOLD: u32 =
    spur_graph::mcp::INCREMENTAL_FAILURES_BEFORE_FULL_REBUILD;

const SESSION_CACHE_CAPACITY: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OverlayRebuildKey {
    head_oid: String,
    dirty_oid_set_hash: u64,
}

impl OverlayRebuildKey {
    pub(crate) fn from(head_oid: &str, dirty: &BTreeMap<PathBuf, [u8; 20]>) -> Self {
        let mut hasher = DefaultHasher::new();
        dirty.hash(&mut hasher);

        Self {
            head_oid: head_oid.to_owned(),
            dirty_oid_set_hash: hasher.finish(),
        }
    }

    fn cache_dir_name(&self) -> String {
        let head = self
            .head_oid
            .chars()
            .filter(|ch| ch.is_ascii_hexdigit())
            .take(16)
            .collect::<String>();
        let head = if head.is_empty() {
            "unknown".to_owned()
        } else {
            head
        };
        format!("{head}-{:016x}", self.dirty_oid_set_hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OverlaySessionKey {
    worktree: PathBuf,
    rebuild: OverlayRebuildKey,
}

impl OverlaySessionKey {
    fn new(worktree: PathBuf, rebuild: OverlayRebuildKey) -> Self {
        Self { worktree, rebuild }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayBuildMode {
    IncrementalDelta,
    CleanRediff,
}

#[derive(Debug)]
pub(crate) struct OverlayMergeSession {
    base_db_path: PathBuf,
    delta_dir: Option<PathBuf>,
    delta_applied: bool,
    algo_as_of: Option<String>,
}

impl OverlayMergeSession {
    fn delta(base_db_path: PathBuf, delta_dir: PathBuf, algo_as_of: Option<String>) -> Self {
        Self {
            base_db_path,
            delta_dir: Some(delta_dir),
            delta_applied: true,
            algo_as_of,
        }
    }

    fn base_only(base_db_path: PathBuf, algo_as_of: Option<String>) -> Self {
        Self {
            base_db_path,
            delta_dir: None,
            delta_applied: false,
            algo_as_of,
        }
    }

    pub(crate) fn base_db_path(&self) -> &Path {
        &self.base_db_path
    }

    pub(crate) fn delta_dir(&self) -> Option<&Path> {
        self.delta_dir.as_deref()
    }

    pub(crate) fn delta_applied(&self) -> bool {
        self.delta_applied
    }

    pub(crate) fn algo_as_of(&self) -> Option<&str> {
        self.algo_as_of.as_deref()
    }
}

type OverlaySessionCell = OnceCell<Arc<OverlayMergeSession>>;

#[derive(Default)]
struct OverlaySessionCacheState {
    latest_by_worktree: HashMap<PathBuf, OverlaySessionKey>,
    delta_failures_by_key: HashMap<OverlaySessionKey, u32>,
    retained: VecDeque<(OverlaySessionKey, Arc<OverlayMergeSession>)>,
}

pub(crate) struct OverlaySessionCoordinator {
    cells: Mutex<HashMap<OverlaySessionKey, Weak<OverlaySessionCell>>>,
    cache: StdMutex<OverlaySessionCacheState>,
}

impl OverlaySessionCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            cells: Mutex::new(HashMap::new()),
            cache: StdMutex::new(OverlaySessionCacheState::default()),
        }
    }

    pub(crate) async fn get_or_build_session<F, Fut>(
        &self,
        worktree: PathBuf,
        rebuild_key: OverlayRebuildKey,
        base_db_path: PathBuf,
        algo_as_of: Option<String>,
        build: F,
    ) -> Arc<OverlayMergeSession>
    where
        F: FnOnce(OverlayBuildMode) -> Fut,
        Fut: Future<Output = Result<PathBuf>>,
    {
        let key = OverlaySessionKey::new(worktree, rebuild_key);
        self.record_latest_key(&key);

        if let Some(session) = self.retained_session(&key) {
            return session;
        }

        let mode = self.next_build_mode(&key);
        let cell = {
            let mut cells = self.cells.lock().await;
            cells.retain(|_, cell| cell.strong_count() > 0);

            if let Some(cell) = cells.get(&key).and_then(Weak::upgrade) {
                cell
            } else {
                let cell = Arc::new(OverlaySessionCell::new());
                cells.insert(key.clone(), Arc::downgrade(&cell));
                cell
            }
        };

        let build_result: Result<&Arc<OverlayMergeSession>> = cell
            .get_or_try_init(|| async {
                let delta_dir = build(mode).await?;
                Ok(Arc::new(OverlayMergeSession::delta(
                    base_db_path.clone(),
                    delta_dir,
                    algo_as_of.clone(),
                )))
            })
            .await;

        match build_result {
            Ok(session) => {
                self.reset_delta_failures(&key);
                self.retain_session(key, Arc::clone(session))
                    .unwrap_or_else(|| Arc::clone(session))
            }
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    mode = ?mode,
                    "analyst worktree overlay delta build failed; serving base-only session"
                );
                if mode == OverlayBuildMode::CleanRediff {
                    self.reset_delta_failures(&key);
                } else {
                    self.record_delta_failure(&key);
                }
                Arc::new(OverlayMergeSession::base_only(base_db_path, algo_as_of))
            }
        }
    }

    fn next_build_mode(&self, key: &OverlaySessionKey) -> OverlayBuildMode {
        let Ok(cache) = self.cache.lock() else {
            return OverlayBuildMode::IncrementalDelta;
        };
        if cache.delta_failures_by_key.get(key).copied().unwrap_or(0)
            >= DELTA_FAILURE_ESCALATION_THRESHOLD
        {
            OverlayBuildMode::CleanRediff
        } else {
            OverlayBuildMode::IncrementalDelta
        }
    }

    fn record_delta_failure(&self, key: &OverlaySessionKey) -> u32 {
        let Ok(mut cache) = self.cache.lock() else {
            return 0;
        };
        let failures = cache.delta_failures_by_key.entry(key.clone()).or_default();
        *failures = failures.saturating_add(1);
        *failures
    }

    fn reset_delta_failures(&self, key: &OverlaySessionKey) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache.delta_failures_by_key.remove(key);
    }

    fn retained_session(&self, key: &OverlaySessionKey) -> Option<Arc<OverlayMergeSession>> {
        let Ok(mut cache) = self.cache.lock() else {
            return None;
        };
        let position = cache
            .retained
            .iter()
            .position(|(session_key, _)| session_key == key)?;
        let (session_key, session) = cache.retained.remove(position)?;
        cache
            .retained
            .push_back((session_key, Arc::clone(&session)));
        Some(session)
    }

    fn record_latest_key(&self, key: &OverlaySessionKey) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache
            .latest_by_worktree
            .insert(key.worktree.clone(), key.clone());
    }

    fn retain_session(
        &self,
        key: OverlaySessionKey,
        session: Arc<OverlayMergeSession>,
    ) -> Option<Arc<OverlayMergeSession>> {
        let Ok(mut cache) = self.cache.lock() else {
            return Some(session);
        };

        if cache
            .latest_by_worktree
            .get(&key.worktree)
            .is_some_and(|latest_key| latest_key != &key)
        {
            return None;
        }

        if let Some(position) = cache
            .retained
            .iter()
            .position(|(session_key, _)| session_key == &key)
        {
            let (_, retained) = cache.retained.remove(position)?;
            if Arc::ptr_eq(&retained, &session) {
                cache.retained.push_back((key, Arc::clone(&retained)));
                return Some(retained);
            }
        }

        cache.retained.push_back((key, Arc::clone(&session)));
        while cache.retained.len() > SESSION_CACHE_CAPACITY {
            cache.retained.pop_front();
        }
        Some(session)
    }
}

impl Default for OverlaySessionCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

static SHARED_OVERLAY_SESSION_COORDINATOR: OnceLock<Arc<OverlaySessionCoordinator>> =
    OnceLock::new();

pub(crate) fn shared_overlay_session_coordinator() -> Arc<OverlaySessionCoordinator> {
    Arc::clone(
        SHARED_OVERLAY_SESSION_COORDINATOR
            .get_or_init(|| Arc::new(OverlaySessionCoordinator::new())),
    )
}

pub(crate) fn overlay_rebuild_key_for_dirty_worktree(worktree: &Path) -> Option<OverlayRebuildKey> {
    let git = spur_graph::git::detect(worktree)?;
    let dirty = dirty_worktree_oids(worktree).ok()?;
    (!dirty.is_empty()).then(|| OverlayRebuildKey::from(&git.head_oid, &dirty))
}

pub(crate) fn write_delta_for_session(
    worktree: &Path,
    key: &OverlayRebuildKey,
    previous: &spur_graph::GraphIndexArtifact,
    mode: OverlayBuildMode,
) -> Result<PathBuf> {
    let root = worktree.join(".spur").join("analyst-overlays");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("failed to create `{}`", root.display()))?;
    let delta_dir = root.join(key.cache_dir_name());

    if delta_dir.exists() {
        std::fs::remove_dir_all(&delta_dir).with_context(|| {
            format!(
                "failed to clear previous analyst overlay delta `{}`",
                delta_dir.display()
            )
        })?;
    }

    tracing::debug!(
        mode = ?mode,
        delta_dir = %delta_dir.display(),
        "building analyst worktree overlay delta"
    );
    spur_graph::store::write_worktree_delta(previous, worktree, &delta_dir)?;
    Ok(delta_dir)
}

fn dirty_worktree_oids(worktree: &Path) -> Result<BTreeMap<PathBuf, [u8; 20]>> {
    let allowed_extensions = spur_graph::extract::languages::all_supported_extensions();
    let mut dirty = BTreeMap::new();

    for entry in spur_graph::git::status_dirty_paths(worktree)? {
        if !is_supported_path(&entry.path, &allowed_extensions) {
            continue;
        }
        let oid = std::fs::read(worktree.join(&entry.path))
            .ok()
            .and_then(|bytes| parse_git_oid(&spur_graph::git_blob_oid(&bytes)))
            .unwrap_or([0; 20]);
        dirty.insert(PathBuf::from(entry.path), oid);
    }

    Ok(dirty)
}

fn is_supported_path(path: &str, allowed_extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            allowed_extensions
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn parse_git_oid(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut bytes = [0_u8; 20];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(chunk, 16).ok()?;
    }
    Some(bytes)
}

/// Open an in-memory `DuckDB` connection that overlays a worktree delta on top of
/// a read-only base analyst database.
pub fn open_worktree_overlay(base_path: &Path, delta_dir: &Path) -> Result<duckdb::Connection> {
    let base_path = base_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", base_path.display()))?;
    let delta_dir = delta_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", delta_dir.display()))?;

    let conn = duckdb::Connection::open_in_memory()
        .context("failed to open in-memory DuckDB overlay connection")?;
    attach_base_read_only(&conn, &base_path)?;
    create_overlay_views(&conn, &delta_dir)?;
    Ok(conn)
}

fn attach_base_read_only(conn: &duckdb::Connection, base_path: &Path) -> Result<()> {
    conn.execute_batch(&format!(
        "ATTACH '{}' AS base (READ_ONLY);",
        sql_escape_path(base_path)
    ))
    .with_context(|| {
        format!(
            "failed to attach base analyst DuckDB read-only at {}",
            base_path.display()
        )
    })
}

fn create_overlay_views(conn: &duckdb::Connection, delta_dir: &Path) -> Result<()> {
    let nodes_path = delta_path(delta_dir, "nodes.parquet");
    let edges_path = delta_path(delta_dir, "edges.parquet");
    let edges_by_dst_path = delta_edges_by_dst_path(delta_dir);
    let edges_unresolved_path = delta_path(delta_dir, "edges_unresolved.parquet");
    let files_path = delta_path(delta_dir, "files.parquet");
    let file_manifests_path = delta_path(delta_dir, "file_manifests.parquet");
    let tombstones_path = delta_path(delta_dir, "tombstones.parquet");

    conn.execute_batch(&format!(
        r"
        CREATE OR REPLACE TABLE delta_dense_id_map AS
        WITH referenced_ids AS (
          SELECT stable_symbol_id FROM read_parquet('{nodes_path}')
          UNION
          SELECT source_stable_id AS stable_symbol_id FROM read_parquet('{edges_path}')
          UNION
          SELECT target_stable_id FROM read_parquet('{edges_path}')
          UNION
          SELECT source_stable_id FROM read_parquet('{edges_by_dst_path}')
          UNION
          SELECT target_stable_id FROM read_parquet('{edges_by_dst_path}')
          UNION
          SELECT source_stable_id FROM read_parquet('{edges_unresolved_path}')
        )
        SELECT
          stable_symbol_id,
          (SELECT COALESCE(MAX(dense_id), 0) FROM base.node_dense_id_map)
            + ROW_NUMBER() OVER (ORDER BY stable_symbol_id) AS dense_id
        FROM (
          SELECT DISTINCT stable_symbol_id
          FROM referenced_ids
          WHERE stable_symbol_id IS NOT NULL
        );

        CREATE OR REPLACE VIEW delta_node_ids AS
        SELECT stable_symbol_id
        FROM read_parquet('{nodes_path}')
        WHERE stable_symbol_id IS NOT NULL;

        CREATE OR REPLACE VIEW raw_tombstone_ids AS
        SELECT stable_file_id AS stable_symbol_id
        FROM base.tombstones
        WHERE stable_file_id IS NOT NULL
        UNION
        SELECT stable_file_id AS stable_symbol_id
        FROM read_parquet('{tombstones_path}')
        WHERE stable_file_id IS NOT NULL;

        CREATE OR REPLACE VIEW removed_file_paths AS
        SELECT DISTINCT fm.path
        FROM base.file_manifests fm
        WHERE fm.stable_file_id IN (SELECT stable_symbol_id FROM raw_tombstone_ids)
          AND fm.path NOT IN (SELECT path FROM read_parquet('{file_manifests_path}'));

        CREATE OR REPLACE VIEW tombstone_ids AS
        SELECT stable_symbol_id
        FROM raw_tombstone_ids
        UNION
        SELECT stable_symbol_id
        FROM base.nodes
        WHERE file_path IN (SELECT path FROM removed_file_paths);

        CREATE OR REPLACE VIEW tombstones AS
        SELECT *
        FROM base.tombstones
        UNION ALL
        SELECT *
        FROM read_parquet('{tombstones_path}');

        CREATE OR REPLACE VIEW nodes AS
        SELECT *
        FROM base.nodes
        WHERE stable_symbol_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND stable_symbol_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT n.* REPLACE (m.dense_id AS node_id)
        FROM read_parquet('{nodes_path}') n
        JOIN delta_dense_id_map m USING (stable_symbol_id);

        CREATE OR REPLACE VIEW edges AS
        SELECT *
        FROM base.edges
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND target_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id,
          COALESCE(dst_delta.dense_id, dst_base.dense_id) AS dst_id
        )
        FROM read_parquet('{edges_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        LEFT JOIN delta_dense_id_map dst_delta
          ON dst_delta.stable_symbol_id = e.target_stable_id
        LEFT JOIN base.node_dense_id_map dst_base
          ON dst_base.stable_symbol_id = e.target_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL
          AND COALESCE(dst_delta.dense_id, dst_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW edges_by_dst AS
        SELECT *
        FROM base.edges_by_dst
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND target_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id,
          COALESCE(dst_delta.dense_id, dst_base.dense_id) AS dst_id
        )
        FROM read_parquet('{edges_by_dst_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        LEFT JOIN delta_dense_id_map dst_delta
          ON dst_delta.stable_symbol_id = e.target_stable_id
        LEFT JOIN base.node_dense_id_map dst_base
          ON dst_base.stable_symbol_id = e.target_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL
          AND COALESCE(dst_delta.dense_id, dst_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW edges_unresolved AS
        SELECT *
        FROM base.edges_unresolved
        WHERE source_stable_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND source_stable_id NOT IN (SELECT stable_symbol_id FROM delta_node_ids)
        UNION ALL
        SELECT e.* REPLACE (
          COALESCE(src_delta.dense_id, src_base.dense_id) AS src_id
        )
        FROM read_parquet('{edges_unresolved_path}') e
        LEFT JOIN delta_dense_id_map src_delta
          ON src_delta.stable_symbol_id = e.source_stable_id
        LEFT JOIN base.node_dense_id_map src_base
          ON src_base.stable_symbol_id = e.source_stable_id
        WHERE COALESCE(src_delta.dense_id, src_base.dense_id) IS NOT NULL;

        CREATE OR REPLACE VIEW files AS
        SELECT *
        FROM base.files
        WHERE stable_file_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND file_path NOT IN (SELECT file_path FROM read_parquet('{files_path}'))
        UNION ALL
        SELECT *
        FROM read_parquet('{files_path}');

        CREATE OR REPLACE VIEW file_manifests AS
        SELECT *
        FROM base.file_manifests
        WHERE stable_file_id NOT IN (SELECT stable_symbol_id FROM tombstone_ids)
          AND path NOT IN (SELECT path FROM read_parquet('{file_manifests_path}'))
        UNION ALL
        SELECT *
        FROM read_parquet('{file_manifests_path}');
        "
    ))
    .with_context(|| {
        format!(
            "failed to create worktree overlay views for delta {}",
            delta_dir.display()
        )
    })
}

fn delta_path(delta_dir: &Path, file_name: &str) -> String {
    sql_escape_path(&delta_dir.join(file_name))
}

fn delta_edges_by_dst_path(delta_dir: &Path) -> String {
    let path = delta_dir.join("edges_by_dst.parquet");
    let path = if path.exists() {
        path
    } else {
        delta_dir.join("edges.parquet")
    };
    sql_escape_path(&path)
}

fn sql_escape_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use super::*;

    fn rebuild_key(head_oid: &str, dirty_byte: u8) -> OverlayRebuildKey {
        let mut dirty = BTreeMap::new();
        dirty.insert(PathBuf::from("src/lib.rs"), [dirty_byte; 20]);
        OverlayRebuildKey::from(head_oid, &dirty)
    }

    #[tokio::test]
    async fn merge_session_reuses_cached_delta_for_same_dirty_key() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let coordinator = OverlaySessionCoordinator::new();
        let worktree = tempdir.path().join("repo");
        let base_db = tempdir.path().join("analyst.duckdb");
        let delta_dir = tempdir.path().join("delta-one");
        let key = rebuild_key("head-a", 1);
        let builds = AtomicUsize::new(0);

        let first = coordinator
            .get_or_build_session(
                worktree.clone(),
                key.clone(),
                base_db.clone(),
                Some("base-hash".to_owned()),
                |_| {
                    builds.fetch_add(1, Ordering::SeqCst);
                    let delta_dir = delta_dir.clone();
                    async move { Ok(delta_dir) }
                },
            )
            .await;
        let second = coordinator
            .get_or_build_session(worktree, key, base_db, Some("base-hash".to_owned()), |_| {
                builds.fetch_add(1, Ordering::SeqCst);
                let delta_dir = delta_dir.clone();
                async move { Ok(delta_dir) }
            })
            .await;

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "same dirty key should reuse the retained merge session"
        );
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.delta_applied());
        assert_eq!(first.algo_as_of(), Some("base-hash"));
    }

    #[tokio::test]
    async fn merge_session_rebuilds_after_dirty_key_changes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let coordinator = OverlaySessionCoordinator::new();
        let worktree = tempdir.path().join("repo");
        let base_db = tempdir.path().join("analyst.duckdb");
        let builds = AtomicUsize::new(0);

        let first = coordinator
            .get_or_build_session(
                worktree.clone(),
                rebuild_key("head-a", 1),
                base_db.clone(),
                Some("base-hash".to_owned()),
                |_| {
                    let attempt = builds.fetch_add(1, Ordering::SeqCst);
                    let delta_dir = tempdir.path().join(format!("delta-{attempt}"));
                    async move { Ok(delta_dir) }
                },
            )
            .await;
        let second = coordinator
            .get_or_build_session(
                worktree,
                rebuild_key("head-a", 2),
                base_db,
                Some("base-hash".to_owned()),
                |_| {
                    let attempt = builds.fetch_add(1, Ordering::SeqCst);
                    let delta_dir = tempdir.path().join(format!("delta-{attempt}"));
                    async move { Ok(delta_dir) }
                },
            )
            .await;

        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "changed dirty key should build a new merge session"
        );
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(second.delta_applied());
    }

    #[tokio::test]
    async fn persistent_delta_failures_escalate_to_clean_rediff_attempt() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let coordinator = OverlaySessionCoordinator::new();
        let worktree = tempdir.path().join("repo");
        let base_db = tempdir.path().join("analyst.duckdb");
        let delta_dir = tempdir.path().join("delta");
        let key = rebuild_key("head-a", 1);
        let modes = Arc::new(Mutex::new(Vec::new()));

        for attempt in 1..=DELTA_FAILURE_ESCALATION_THRESHOLD {
            let modes = Arc::clone(&modes);
            let session = coordinator
                .get_or_build_session(
                    worktree.clone(),
                    key.clone(),
                    base_db.clone(),
                    Some("base-hash".to_owned()),
                    move |mode| {
                        modes.lock().expect("modes").push(mode);
                        async move { Err(anyhow!("forced delta failure")) }
                    },
                )
                .await;
            assert!(
                !session.delta_applied(),
                "attempt {attempt} should degrade to a base-only session"
            );
        }

        let modes_for_success = Arc::clone(&modes);
        let recovered = coordinator
            .get_or_build_session(
                worktree,
                key,
                base_db,
                Some("base-hash".to_owned()),
                move |mode| {
                    modes_for_success.lock().expect("modes").push(mode);
                    let delta_dir = delta_dir.clone();
                    async move { Ok(delta_dir) }
                },
            )
            .await;

        assert!(recovered.delta_applied());
        let modes = modes.lock().expect("modes");
        assert_eq!(
            &modes[..DELTA_FAILURE_ESCALATION_THRESHOLD as usize],
            vec![OverlayBuildMode::IncrementalDelta; DELTA_FAILURE_ESCALATION_THRESHOLD as usize]
                .as_slice()
        );
        assert_eq!(
            modes[DELTA_FAILURE_ESCALATION_THRESHOLD as usize],
            OverlayBuildMode::CleanRediff,
            "the call after the threshold should force a clean re-diff attempt"
        );
    }
}
