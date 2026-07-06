use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use anyhow::{Context as _, Result};
use tokio::sync::{Mutex, OnceCell};

mod analytics;
mod duckdb;
#[cfg(test)]
mod session_tests;

pub use duckdb::open_worktree_overlay;

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
        let cell = self.session_cell_for_key(&key).await;
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

        self.session_from_build_result(key, mode, base_db_path, algo_as_of, build_result)
    }

    async fn session_cell_for_key(&self, key: &OverlaySessionKey) -> Arc<OverlaySessionCell> {
        let mut cells = self.cells.lock().await;
        cells.retain(|_, cell| cell.strong_count() > 0);

        if let Some(cell) = cells.get(key).and_then(Weak::upgrade) {
            return cell;
        }
        let cell = Arc::new(OverlaySessionCell::new());
        cells.insert(key.clone(), Arc::downgrade(&cell));
        cell
    }

    fn session_from_build_result(
        &self,
        key: OverlaySessionKey,
        mode: OverlayBuildMode,
        base_db_path: PathBuf,
        algo_as_of: Option<String>,
        build_result: Result<&Arc<OverlayMergeSession>>,
    ) -> Arc<OverlayMergeSession> {
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
