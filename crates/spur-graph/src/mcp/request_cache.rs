use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::WorktreeGitMetadata;
use crate::{GraphIndexArtifact, ParquetClient};

const GIT_METADATA_CACHE_TTL: Duration = Duration::from_millis(5_000);
const PARQUET_CLIENT_CACHE_CAPACITY: usize = 8;
const OVERLAY_DELTA_CACHE_CAPACITY: usize = 1;

static GIT_METADATA_CACHE: OnceLock<Mutex<GitMetadataCache>> = OnceLock::new();
static PARQUET_CLIENT_CACHE: OnceLock<Mutex<ParquetClientCache>> = OnceLock::new();
static OVERLAY_DELTA_CACHE: OnceLock<Mutex<OverlayDeltaCache>> = OnceLock::new();

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
    let built = build()?;
    overlay_delta_cache_insert(cache_key, built.clone());
    Ok(built)
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
    use super::*;
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

    static OVERLAY_DELTA_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn overlay_delta_cache_reuses_build_for_same_changed_files_fingerprint() {
        let _guard = OVERLAY_DELTA_TEST_LOCK
            .lock()
            .expect("overlay delta test lock");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        let builds = AtomicUsize::new(0);

        let first = overlay_delta(worktree, 1, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("one"))
        })
        .expect("first overlay delta");
        let second = overlay_delta(worktree, 1, || {
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
        let _guard = OVERLAY_DELTA_TEST_LOCK
            .lock()
            .expect("overlay delta test lock");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let worktree = tempdir.path();
        let builds = AtomicUsize::new(0);

        overlay_delta(worktree, 1, || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok(dummy_overlay_delta("one"))
        })
        .expect("first overlay delta");
        overlay_delta(worktree, 2, || {
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
