use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::WorktreeGitMetadata;
use crate::{GraphIndexArtifact, ParquetClient};

const GIT_METADATA_CACHE_TTL: Duration = Duration::from_millis(5_000);
const PARQUET_CLIENT_CACHE_CAPACITY: usize = 8;
const OVERLAY_DELTA_CACHE_CAPACITY: usize = PARQUET_CLIENT_CACHE_CAPACITY;

static GIT_METADATA_CACHE: OnceLock<Mutex<GitMetadataCache>> = OnceLock::new();
static PARQUET_CLIENT_CACHE: OnceLock<Mutex<ParquetClientCache>> = OnceLock::new();
static OVERLAY_DELTA_CACHE: OnceLock<Mutex<OverlayDeltaCache>> = OnceLock::new();
static OVERLAY_DELTA_IN_FLIGHT: OnceLock<
    Mutex<HashMap<OverlayDeltaCacheKey, Arc<OverlayInFlight>>>,
> = OnceLock::new();

pub(super) fn git_metadata_get(
    worktree: &Path,
    indexed_head_oid: Option<&str>,
    now: Instant,
) -> Option<WorktreeGitMetadata> {
    let stamp = GitMetadataCacheStamp::from_worktree(worktree, indexed_head_oid);
    let Ok(cache) = git_cache().lock() else {
        return None;
    };
    cache.get(worktree, &stamp, now)
}

pub(super) fn git_metadata_insert(
    worktree: &Path,
    indexed_head_oid: Option<&str>,
    now: Instant,
    value: WorktreeGitMetadata,
) {
    let stamp = GitMetadataCacheStamp::from_worktree(worktree, indexed_head_oid);
    let Ok(mut cache) = git_cache().lock() else {
        return;
    };
    cache.insert(worktree.to_path_buf(), stamp, now, value);
}

pub(super) fn parquet_client(path: &Path) -> anyhow::Result<Arc<ParquetClient>> {
    let key = ParquetClientCacheKey::from_path(path);
    if let Some(client) = parquet_cache_get(&key) {
        return Ok(client);
    }
    let client = Arc::new(ParquetClient::open(path)?);
    parquet_cache_insert(key, Arc::clone(&client));
    Ok(client)
}

#[derive(Clone)]
pub(super) struct CachedOverlayDelta {
    pub artifact: Arc<GraphIndexArtifact>,
    pub shadowed: HashSet<String>,
}

pub(super) fn overlay_delta(
    worktree: &Path,
    changed_files_fingerprint: u64,
    build: impl FnOnce() -> anyhow::Result<CachedOverlayDelta>,
) -> anyhow::Result<CachedOverlayDelta> {
    let cache_key = OverlayDeltaCacheKey {
        worktree: worktree.to_path_buf(),
        changed_files_fingerprint,
    };
    if let Some(cached) = overlay_delta_cache_get(&cache_key) {
        return Ok(cached);
    }

    let (cell, leader) = overlay_in_flight_cell(cache_key.clone())?;
    if !leader {
        return wait_for_overlay_in_flight(&cell);
    }

    let result = build();
    finish_overlay_in_flight(&cache_key, &cell, &result);
    if let Ok(built) = &result {
        overlay_delta_cache_insert(cache_key, built.clone());
    }
    result
}

struct OverlayInFlight {
    done: Mutex<Option<Result<CachedOverlayDelta, String>>>,
    cv: Condvar,
}

fn overlay_in_flight_map() -> &'static Mutex<HashMap<OverlayDeltaCacheKey, Arc<OverlayInFlight>>> {
    OVERLAY_DELTA_IN_FLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

fn overlay_in_flight_cell(
    cache_key: OverlayDeltaCacheKey,
) -> anyhow::Result<(Arc<OverlayInFlight>, bool)> {
    let mut map = overlay_in_flight_map()
        .lock()
        .map_err(|_| anyhow::anyhow!("overlay in-flight map lock poisoned"))?;
    if let Some(cell) = map.get(&cache_key) {
        return Ok((Arc::clone(cell), false));
    }
    let cell = Arc::new(OverlayInFlight {
        done: Mutex::new(None),
        cv: Condvar::new(),
    });
    map.insert(cache_key, Arc::clone(&cell));
    Ok((cell, true))
}

fn wait_for_overlay_in_flight(cell: &OverlayInFlight) -> anyhow::Result<CachedOverlayDelta> {
    let mut done = cell
        .done
        .lock()
        .map_err(|_| anyhow::anyhow!("overlay in-flight result lock poisoned"))?;
    while done.is_none() {
        done = cell
            .cv
            .wait(done)
            .map_err(|_| anyhow::anyhow!("overlay in-flight wait poisoned"))?;
    }
    match done.as_ref() {
        Some(Ok(value)) => Ok(value.clone()),
        Some(Err(error)) => Err(anyhow::anyhow!("{error}")),
        None => Err(anyhow::anyhow!(
            "overlay in-flight result missing after wait"
        )),
    }
}

fn finish_overlay_in_flight(
    cache_key: &OverlayDeltaCacheKey,
    cell: &OverlayInFlight,
    result: &anyhow::Result<CachedOverlayDelta>,
) {
    if let Ok(mut done) = cell.done.lock() {
        *done = Some(match result {
            Ok(value) => Ok(value.clone()),
            Err(error) => Err(error.to_string()),
        });
        cell.cv.notify_all();
    }
    if let Ok(mut map) = overlay_in_flight_map().lock() {
        map.remove(cache_key);
    }
}

fn git_cache() -> &'static Mutex<GitMetadataCache> {
    GIT_METADATA_CACHE.get_or_init(|| Mutex::new(GitMetadataCache::default()))
}

fn parquet_cache() -> &'static Mutex<ParquetClientCache> {
    PARQUET_CLIENT_CACHE
        .get_or_init(|| Mutex::new(ParquetClientCache::new(PARQUET_CLIENT_CACHE_CAPACITY)))
}

fn overlay_delta_cache() -> &'static Mutex<OverlayDeltaCache> {
    OVERLAY_DELTA_CACHE
        .get_or_init(|| Mutex::new(OverlayDeltaCache::new(OVERLAY_DELTA_CACHE_CAPACITY)))
}

fn parquet_cache_get(key: &ParquetClientCacheKey) -> Option<Arc<ParquetClient>> {
    let Ok(mut cache) = parquet_cache().lock() else {
        return None;
    };
    cache.get(key)
}

fn parquet_cache_insert(key: ParquetClientCacheKey, client: Arc<ParquetClient>) {
    let Ok(mut cache) = parquet_cache().lock() else {
        return;
    };
    cache.insert(key, client);
}

fn overlay_delta_cache_get(key: &OverlayDeltaCacheKey) -> Option<CachedOverlayDelta> {
    let Ok(mut cache) = overlay_delta_cache().lock() else {
        return None;
    };
    cache.get(key)
}

fn overlay_delta_cache_insert(key: OverlayDeltaCacheKey, value: CachedOverlayDelta) {
    let Ok(mut cache) = overlay_delta_cache().lock() else {
        return;
    };
    cache.insert(key, value);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitMetadataCacheStamp {
    indexed_head_oid: Option<String>,
    head_mtime_ns: i128,
    index_mtime_ns: i128,
}

impl GitMetadataCacheStamp {
    fn from_worktree(worktree: &Path, indexed_head_oid: Option<&str>) -> Self {
        let git_dir = worktree.join(".git");
        Self {
            indexed_head_oid: indexed_head_oid.map(str::to_owned),
            head_mtime_ns: path_mtime_ns(&git_dir.join("HEAD")),
            index_mtime_ns: path_mtime_ns(&git_dir.join("index")),
        }
    }
}

struct GitMetadataCacheEntry {
    stamp: GitMetadataCacheStamp,
    fetched_at: Instant,
    value: WorktreeGitMetadata,
}

#[derive(Default)]
struct GitMetadataCache {
    entries: HashMap<PathBuf, GitMetadataCacheEntry>,
}

impl GitMetadataCache {
    fn get(
        &self,
        worktree: &Path,
        stamp: &GitMetadataCacheStamp,
        now: Instant,
    ) -> Option<WorktreeGitMetadata> {
        let entry = self.entries.get(worktree)?;
        if entry.stamp != *stamp {
            return None;
        }
        if now.saturating_duration_since(entry.fetched_at) >= GIT_METADATA_CACHE_TTL {
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(
        &mut self,
        worktree: PathBuf,
        stamp: GitMetadataCacheStamp,
        now: Instant,
        value: WorktreeGitMetadata,
    ) {
        self.entries.insert(
            worktree,
            GitMetadataCacheEntry {
                stamp,
                fetched_at: now,
                value,
            },
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ParquetClientCacheKey {
    path: PathBuf,
    manifest_mtime_ns: i128,
}

impl ParquetClientCacheKey {
    fn from_path(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            manifest_mtime_ns: path_mtime_ns(&path.join("manifest.json")),
        }
    }
}

struct ParquetClientCache {
    entries: HashMap<ParquetClientCacheKey, Arc<ParquetClient>>,
    lru: VecDeque<ParquetClientCacheKey>,
    capacity: usize,
}

impl ParquetClientCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            capacity,
        }
    }

    fn get(&mut self, key: &ParquetClientCacheKey) -> Option<Arc<ParquetClient>> {
        let client = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(client)
    }

    fn insert(&mut self, key: ParquetClientCacheKey, client: Arc<ParquetClient>) {
        self.entries.insert(key.clone(), client);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(expired) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&expired);
        }
    }

    fn touch(&mut self, key: &ParquetClientCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OverlayDeltaCacheKey {
    worktree: PathBuf,
    changed_files_fingerprint: u64,
}

struct OverlayDeltaCache {
    entries: HashMap<OverlayDeltaCacheKey, CachedOverlayDelta>,
    lru: VecDeque<OverlayDeltaCacheKey>,
    capacity: usize,
}

impl OverlayDeltaCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            capacity,
        }
    }

    fn get(&mut self, key: &OverlayDeltaCacheKey) -> Option<CachedOverlayDelta> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: OverlayDeltaCacheKey, value: CachedOverlayDelta) {
        self.entries.insert(key.clone(), value);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(expired) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&expired);
        }
    }

    fn touch(&mut self, key: &OverlayDeltaCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

fn path_mtime_ns(path: &Path) -> i128 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::overlay_snapshot::{
        self, normalized_changed_set_fingerprint, OverlayPathState, SnapshotBase, SnapshotIdentity,
    };
    use super::*;
    use anyhow::Context as _;
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use crate::schema::{GraphIndexHeader, GRAPH_INDEX_VERSION_TEMPORAL};
    use crate::{write_artifact_parquet, GraphIndexArtifact, WriteOptions};

    fn sample_git_metadata(head: &str) -> WorktreeGitMetadata {
        WorktreeGitMetadata {
            head_oid: head.to_owned(),
            has_uncommitted_changes: true,
            supplemental_changed: Vec::new(),
        }
    }

    fn git_metadata_cached(
        worktree: &Path,
        indexed_head_oid: Option<&str>,
        now: Instant,
        fetch: impl FnOnce() -> Option<WorktreeGitMetadata>,
    ) -> Option<WorktreeGitMetadata> {
        if let Some(cached) = git_metadata_get(worktree, indexed_head_oid, now) {
            return Some(cached);
        }
        let fetched = fetch()?;
        git_metadata_insert(worktree, indexed_head_oid, now, fetched.clone());
        Some(fetched)
    }

    fn write_empty_parquet(dir: &Path) -> PathBuf {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_owned(),
                content_hash_blake3: None,
            },
            manifest_version: "test-manifest".to_owned(),
            graph_content_hash: "cache-test-hash".to_owned(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            file_node_ids: Vec::new(),
            symbols: Vec::new(),
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        };
        write_artifact_parquet(&artifact, dir, WriteOptions::default(), Vec::new())
            .expect("write empty parquet artifact")
    }

    #[test]
    fn git_metadata_cache_reuses_fetch_within_ttl() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        fs::create_dir_all(worktree.join(".git")).expect("git dir");
        fs::write(worktree.join(".git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let fetches = AtomicUsize::new(0);
        let now = Instant::now();

        let first = git_metadata_cached(worktree, None, now, || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("abc"))
        });
        let second = git_metadata_cached(worktree, None, now + Duration::from_millis(10), || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("def"))
        });

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(
            first.as_ref().map(|meta| meta.head_oid.as_str()),
            Some("abc")
        );
        assert_eq!(
            second.as_ref().map(|meta| meta.head_oid.as_str()),
            Some("abc")
        );
    }

    #[test]
    fn git_metadata_cache_refetches_after_ttl() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        fs::create_dir_all(worktree.join(".git")).expect("git dir");
        fs::write(worktree.join(".git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let fetches = AtomicUsize::new(0);
        let now = Instant::now();

        let first = git_metadata_cached(worktree, None, now, || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("abc"))
        });
        let second =
            git_metadata_cached(worktree, None, now + Duration::from_millis(5_001), || {
                fetches.fetch_add(1, Ordering::SeqCst);
                Some(sample_git_metadata("def"))
            });

        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert_eq!(first.unwrap().head_oid, "abc");
        assert_eq!(second.unwrap().head_oid, "def");
    }

    #[test]
    fn git_metadata_cache_refetches_when_head_mtime_changes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        let git_dir = worktree.join(".git");
        fs::create_dir_all(&git_dir).expect("git dir");
        let head = git_dir.join("HEAD");
        fs::write(&head, "ref: refs/heads/main\n").expect("HEAD");
        let fetches = AtomicUsize::new(0);
        let now = Instant::now();

        git_metadata_cached(worktree, None, now, || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("abc"))
        });
        fs::write(&head, "ref: refs/heads/topic\n").expect("rewrite HEAD");
        let file = fs::File::open(&head).expect("open HEAD");
        file.set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
            .expect("bump HEAD mtime");

        git_metadata_cached(worktree, None, now + Duration::from_millis(10), || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("def"))
        });

        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn git_metadata_cache_refetches_when_indexed_head_changes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        fs::create_dir_all(worktree.join(".git")).expect("git dir");
        fs::write(worktree.join(".git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let fetches = AtomicUsize::new(0);
        let now = Instant::now();

        git_metadata_cached(worktree, None, now, || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Some(sample_git_metadata("abc"))
        });
        let refreshed = git_metadata_cached(
            worktree,
            Some("indexed-base"),
            now + Duration::from_millis(10),
            || {
                fetches.fetch_add(1, Ordering::SeqCst);
                Some(sample_git_metadata("def"))
            },
        );

        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert_eq!(refreshed.unwrap().head_oid, "def");
    }

    #[test]
    fn git_metadata_cache_replaces_prior_stamp_for_worktree() {
        let mut cache = GitMetadataCache::default();
        let worktree = PathBuf::from("/repo");
        let now = Instant::now();
        let first = GitMetadataCacheStamp {
            indexed_head_oid: Some("base-a".into()),
            head_mtime_ns: 1,
            index_mtime_ns: 1,
        };
        let second = GitMetadataCacheStamp {
            indexed_head_oid: Some("base-b".into()),
            head_mtime_ns: 2,
            index_mtime_ns: 2,
        };

        cache.insert(worktree.clone(), first, now, sample_git_metadata("abc"));
        cache.insert(
            worktree,
            second,
            now + Duration::from_millis(1),
            sample_git_metadata("def"),
        );

        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn parquet_client_cache_reuses_open_for_same_path() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dir = write_empty_parquet(tempdir.path());
        let first = parquet_client(&dir).expect("first open");
        let second = parquet_client(&dir).expect("second open");
        assert!(
            Arc::ptr_eq(&first, &second),
            "same artifact path should reuse the cached ParquetClient"
        );
    }

    fn empty_artifact(hash: &str) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_owned(),
                content_hash_blake3: None,
            },
            manifest_version: "test-manifest".to_owned(),
            graph_content_hash: hash.to_owned(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            file_node_ids: Vec::new(),
            symbols: Vec::new(),
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        }
    }

    fn dummy_overlay_delta(tag: &str) -> CachedOverlayDelta {
        CachedOverlayDelta {
            artifact: Arc::new(empty_artifact(tag)),
            shadowed: HashSet::from([tag.to_owned()]),
        }
    }

    fn cache_identity(worktree: &Path, tag: u8) -> SnapshotIdentity {
        overlay_snapshot::test_snapshot_identity(worktree, tag)
    }

    fn overlay_delta_for_identity_test(
        identity: &SnapshotIdentity,
        build: impl FnOnce() -> anyhow::Result<CachedOverlayDelta>,
    ) -> anyhow::Result<CachedOverlayDelta> {
        let fingerprint = u64::from_le_bytes(
            identity.normalized_changed_set_fingerprint[..8]
                .try_into()
                .expect("eight-byte legacy fingerprint"),
        );
        overlay_delta(&identity.canonical_worktree, fingerprint, build)
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn snapshot_fixture(root: &Path) -> SnapshotBase {
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "cache@example.com"]);
        run_git(root, &["config", "user.name", "Cache Test"]);
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::write(root.join("src/lib.rs"), "pub fn cached() {}\n").expect("source");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let file_oids = crate::git::ls_files_with_oids(root)
            .expect("tracked files")
            .into_iter()
            .map(|entry| (entry.path, entry.content_oid))
            .collect::<BTreeMap<_, _>>();
        SnapshotBase {
            indexed_graph_content_hash: "no-ttl-graph".to_owned(),
            indexed_head_oid: Some(crate::git::rev_parse_head(root).expect("HEAD")),
            file_oids,
        }
    }

    static OVERLAY_DELTA_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn overlay_delta_test_lock() -> std::sync::MutexGuard<'static, ()> {
        OVERLAY_DELTA_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn overlay_delta_cache_reuses_build_for_same_changed_files_fingerprint() {
        let _guard = overlay_delta_test_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        let identity = cache_identity(worktree, 1);
        let builds = AtomicUsize::new(0);

        let first = overlay_delta_for_identity_test(&identity, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("one"))
        })
        .expect("first overlay delta");
        let second = overlay_delta_for_identity_test(&identity, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("two"))
        })
        .expect("second overlay delta");

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "same worktree and changed-files fingerprint should reuse the cached overlay extract"
        );
        assert!(Arc::ptr_eq(&first.artifact, &second.artifact));
        assert_eq!(first.shadowed, second.shadowed);
    }

    #[test]
    fn overlay_delta_cache_rebuilds_when_changed_files_fingerprint_changes() {
        let _guard = overlay_delta_test_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        let builds = AtomicUsize::new(0);

        overlay_delta_for_identity_test(&cache_identity(worktree, 1), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("one"))
        })
        .expect("first overlay delta");
        overlay_delta_for_identity_test(&cache_identity(worktree, 2), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("two"))
        })
        .expect("second overlay delta");

        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "changed file contents should extract a new overlay delta"
        );
    }

    #[test]
    fn overlay_delta_cache_keeps_per_worktree_entries_when_switching_projects() {
        let _guard = overlay_delta_test_lock();
        let spur = tempfile::tempdir().expect("spur worktree");
        let notebook = tempfile::tempdir().expect("notebook worktree");
        let otobank = tempfile::tempdir().expect("otobank worktree");
        let builds = AtomicUsize::new(0);

        overlay_delta_for_identity_test(&cache_identity(spur.path(), 1), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("spur"))
        })
        .expect("spur overlay");
        overlay_delta_for_identity_test(&cache_identity(notebook.path(), 1), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("notebook"))
        })
        .expect("notebook overlay");
        overlay_delta_for_identity_test(&cache_identity(otobank.path(), 1), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("otobank"))
        })
        .expect("otobank overlay");
        overlay_delta_for_identity_test(&cache_identity(spur.path(), 1), || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("spur-again"))
        })
        .expect("spur overlay reuse after switching projects");

        assert_eq!(
            OVERLAY_DELTA_CACHE_CAPACITY, PARQUET_CLIENT_CACHE_CAPACITY,
            "overlay cache must keep one live delta per cached parquet project"
        );
        assert_eq!(
            builds.load(Ordering::SeqCst),
            3,
            "switching projects must reuse each worktree overlay instead of rebuilding after eviction"
        );
    }

    #[test]
    fn overlay_delta_singleflight_shares_in_flight_build_for_same_key() {
        let _guard = overlay_delta_test_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path().to_path_buf();
        let identity = cache_identity(&worktree, 7);
        let builds = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let spawn = |builds: Arc<AtomicUsize>,
                     barrier: Arc<std::sync::Barrier>,
                     identity: SnapshotIdentity| {
            std::thread::spawn(move || {
                barrier.wait();
                overlay_delta_for_identity_test(&identity, || {
                    std::thread::sleep(Duration::from_millis(50));
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(dummy_overlay_delta("shared"))
                })
            })
        };

        let first = spawn(Arc::clone(&builds), Arc::clone(&barrier), identity.clone());
        let second = spawn(builds.clone(), barrier, identity);
        let first = first
            .join()
            .expect("first overlay thread")
            .expect("first overlay");
        let second = second
            .join()
            .expect("second overlay thread")
            .expect("second overlay");

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "concurrent overlay extracts for the same worktree fingerprint must share one in-flight build"
        );
        assert!(Arc::ptr_eq(&first.artifact, &second.artifact));
        assert_eq!(first.shadowed, second.shadowed);
    }

    #[test]
    fn oid_addressed_layers_reuse_after_more_than_five_seconds() {
        let _guard = overlay_delta_test_lock();
        let parquet_tempdir = tempfile::tempdir().expect("parquet tempdir");
        let parquet_dir = write_empty_parquet(parquet_tempdir.path());
        let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
        let first_file_oids = parquet.file_oids().expect("first file OID read");
        fs::remove_file(parquet_dir.join("file_manifests.parquet"))
            .expect("remove backing file after first read");

        let worktree_tempdir = tempfile::tempdir().expect("worktree tempdir");
        let worktree = worktree_tempdir.path();
        let base = snapshot_fixture(worktree);
        let extensions = crate::extract::languages::all_supported_extensions();
        let first_snapshot = overlay_snapshot::snapshot(worktree, base.clone(), &extensions)
            .expect("first validated snapshot");
        let identity = first_snapshot.identity.clone().expect("complete identity");
        let builds = AtomicUsize::new(0);
        let first_delta = overlay_delta_for_identity_test(&identity, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("retained"))
        })
        .expect("first delta");

        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(6_001));
        eprintln!("no-TTL retention elapsed={:?}", started.elapsed());

        let second_file_oids = parquet
            .file_oids()
            .expect("same opened manifest reuses file OIDs after old TTL");
        let second_snapshot = overlay_snapshot::snapshot(worktree, base, &extensions)
            .expect("same validated snapshot after old TTL");
        let second_delta = overlay_delta_for_identity_test(&identity, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("rebuilt"))
        })
        .expect("same cached delta after old TTL");

        assert_eq!(second_file_oids, first_file_oids);
        assert!(second_snapshot.measurements.snapshot_reused);
        assert_eq!(second_snapshot.identity.as_ref(), Some(&identity));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first_delta.artifact, &second_delta.artifact));
    }

    #[test]
    fn overlay_delta_invalidates_every_complete_identity_component() {
        let _guard = overlay_delta_test_lock();
        let first_parent = tempfile::tempdir().expect("first parent");
        let second_parent = tempfile::tempdir().expect("second parent");
        let first_worktree = first_parent.path().join("same-relative-name");
        let second_worktree = second_parent.path().join("same-relative-name");
        fs::create_dir_all(&first_worktree).expect("first worktree");
        fs::create_dir_all(&second_worktree).expect("second worktree");
        let baseline = cache_identity(&first_worktree, 1);
        let other_identity = cache_identity(&second_worktree, 1);
        let other_index_identity = cache_identity(&first_worktree, 2).index_identity;

        let fingerprint = |entries: Vec<(String, OverlayPathState)>| {
            normalized_changed_set_fingerprint(entries.iter().map(|(path, state)| (path, state)))
        };
        let mut variants = Vec::new();
        let mut graph = baseline.clone();
        graph.indexed_graph_content_hash = "graph-changed".to_owned();
        variants.push(("graph", graph));
        let mut indexed_head = baseline.clone();
        indexed_head.indexed_head_oid = Some("indexed-head-changed".to_owned());
        variants.push(("indexed-head", indexed_head));
        let mut current_head = baseline.clone();
        current_head.current_head_oid = "current-head-changed".to_owned();
        variants.push(("current-head", current_head));
        let mut index = baseline.clone();
        index.index_identity = other_index_identity;
        variants.push(("index", index));
        let mut changed_oid = baseline.clone();
        changed_oid.normalized_changed_set_fingerprint = fingerprint(vec![(
            "src/lib.rs".to_owned(),
            OverlayPathState::Tracked("oid-v2".to_owned()),
        )]);
        variants.push(("changed-oid", changed_oid));
        let mut deletion = baseline.clone();
        deletion.normalized_changed_set_fingerprint =
            fingerprint(vec![("src/lib.rs".to_owned(), OverlayPathState::Deleted)]);
        variants.push(("deletion", deletion));
        let mut rename = baseline.clone();
        rename.normalized_changed_set_fingerprint = fingerprint(vec![
            ("src/lib.rs".to_owned(), OverlayPathState::Deleted),
            (
                "src/renamed.rs".to_owned(),
                OverlayPathState::Tracked("oid-v1".to_owned()),
            ),
        ]);
        variants.push(("rename", rename));
        let mut other_worktree = baseline.clone();
        other_worktree.canonical_worktree = other_identity.canonical_worktree;
        variants.push(("canonical-worktree", other_worktree));

        let builds = AtomicUsize::new(0);
        overlay_delta_for_identity_test(&baseline, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("baseline"))
        })
        .expect("baseline");
        for (label, identity) in &variants {
            let actual = overlay_delta_for_identity_test(identity, || {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(dummy_overlay_delta(label))
            })
            .unwrap_or_else(|error| panic!("{label} rebuild failed: {error:#}"));
            assert_eq!(
                actual.artifact.graph_content_hash, *label,
                "{label} must never reuse a mismatched complete identity"
            );
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1 + variants.len());
    }

    #[test]
    fn overlay_delta_capacity_evicts_without_returning_mismatched_entries() {
        let _guard = overlay_delta_test_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let builds = AtomicUsize::new(0);
        let identities = (0..=OVERLAY_DELTA_CACHE_CAPACITY)
            .map(|index| {
                let mut identity = cache_identity(tempdir.path(), 42);
                identity.current_head_oid = format!("head-{index}");
                identity
            })
            .collect::<Vec<_>>();

        for (index, identity) in identities.iter().enumerate() {
            let tag = format!("entry-{index}");
            let actual = overlay_delta_for_identity_test(identity, || {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(dummy_overlay_delta(&tag))
            })
            .expect("insert cache entry");
            assert_eq!(actual.artifact.graph_content_hash, tag);
        }
        assert_eq!(builds.load(Ordering::SeqCst), identities.len());

        let rebuilt = overlay_delta_for_identity_test(&identities[0], || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("entry-0-rebuilt"))
        })
        .expect("oldest entry rebuild");
        assert_eq!(rebuilt.artifact.graph_content_hash, "entry-0-rebuilt");
        assert_eq!(builds.load(Ordering::SeqCst), identities.len() + 1);
    }

    #[test]
    fn overlay_delta_singleflight_followers_receive_exact_leader_error() {
        let _guard = overlay_delta_test_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let identity = cache_identity(tempdir.path(), 99);
        let builds = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let spawn = |builds: Arc<AtomicUsize>,
                     barrier: Arc<std::sync::Barrier>,
                     identity: SnapshotIdentity| {
            std::thread::spawn(move || {
                barrier.wait();
                let result = overlay_delta_for_identity_test(&identity, || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Err::<CachedOverlayDelta, _>(anyhow::anyhow!("inner exact error"))
                        .context("outer exact error")
                });
                result.err().expect("shared error")
            })
        };
        let first = spawn(Arc::clone(&builds), Arc::clone(&barrier), identity.clone());
        let second = spawn(builds.clone(), barrier, identity);
        let errors = [
            first.join().expect("first error thread"),
            second.join().expect("second error thread"),
        ];

        eprintln!(
            "singleflight exact errors: {:?}",
            errors
                .iter()
                .map(|error| format!("{error:#}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(errors
            .iter()
            .all(|error| format!("{error:#}") == "outer exact error: inner exact error"));
    }

    #[test]
    fn parquet_client_cache_opens_again_when_manifest_mtime_changes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dir = write_empty_parquet(tempdir.path());
        let first = parquet_client(&dir).expect("first open");
        let manifest = dir.join("manifest.json");
        let file = fs::File::open(&manifest).expect("open manifest");
        file.set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
            .expect("bump manifest mtime");
        let second = parquet_client(&dir).expect("second open");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a newer manifest should open a fresh ParquetClient"
        );
    }
}
