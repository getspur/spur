use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::temporal::{GitSha, TemporalIndex};
use crate::{
    ChangeKind, CommitIndexArtifact, GraphFileManifestEntry, GraphQueryClient, GraphSymbolArtifact,
    OwnedCalleeRecord, OwnedCallerRecord, SearchOptions, SearchResult, SelectorResolution,
    SnapshotKey,
};

const MAX_SEARCH_RESULTS: usize = 200;

type SymbolHistoryKey = (CommitIndexArtifact, String);
type SymbolHistory = Vec<(GitSha, ChangeKind, SnapshotKey)>;

struct RequestMemo<K, V> {
    entries: Mutex<Vec<(K, V)>>,
}

impl<K, V> Default for RequestMemo<K, V> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl<K, V> RequestMemo<K, V>
where
    K: PartialEq,
    V: Clone,
{
    fn entries(&self) -> MutexGuard<'_, Vec<(K, V)>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get(&self, key: &K) -> Option<V> {
        self.entries()
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.clone())
    }

    fn insert(&self, key: K, value: &V) {
        self.entries().push((key, value.clone()));
    }

    fn get_or_insert_with(&self, key: K, load: impl FnOnce() -> V) -> V {
        if let Some(value) = self.get(&key) {
            return value;
        }
        let value = load();
        self.insert(key, &value);
        value
    }

    fn get_or_try_insert_with<E>(
        &self,
        key: K,
        load: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        if let Some(value) = self.get(&key) {
            return Ok(value);
        }
        let value = load()?;
        self.insert(key, &value);
        Ok(value)
    }
}

/// Typed, request-local replay of base graph operations.
///
/// A request executes its handler once against this client before freshness
/// analysis. If a dirty overlay is required, the same client becomes the
/// overlay's base. Repeated operations are then served from these typed memos
/// rather than querying Parquet again. Errors are deliberately not memoized:
/// retaining them would require cloning or reconstructing `anyhow::Error` and
/// would change its source chain.
pub(super) struct RequestReplayClient<'a> {
    base: &'a (dyn GraphQueryClient + Sync),
    searches: RequestMemo<SearchOptions, SearchResult>,
    caller_edges: RequestMemo<String, Vec<OwnedCallerRecord>>,
    unresolved_callers: RequestMemo<Vec<String>, Vec<OwnedCallerRecord>>,
    callee_edges: RequestMemo<String, Vec<OwnedCalleeRecord>>,
    resolutions: RequestMemo<String, SelectorResolution>,
    symbols_by_id: RequestMemo<String, Option<GraphSymbolArtifact>>,
    symbols_by_file: RequestMemo<String, Vec<GraphSymbolArtifact>>,
    symbols_by_files: RequestMemo<Vec<String>, Vec<GraphSymbolArtifact>>,
    symbols_by_path_name: RequestMemo<(String, String), Vec<GraphSymbolArtifact>>,
    file_manifests: RequestMemo<String, Option<GraphFileManifestEntry>>,
    file_exists: RequestMemo<String, bool>,
    temporal_index: OnceLock<Arc<TemporalIndex>>,
    symbol_histories: RequestMemo<SymbolHistoryKey, SymbolHistory>,
}

impl<'a> RequestReplayClient<'a> {
    pub(super) fn new(base: &'a (dyn GraphQueryClient + Sync)) -> Self {
        Self {
            base,
            searches: RequestMemo::default(),
            caller_edges: RequestMemo::default(),
            unresolved_callers: RequestMemo::default(),
            callee_edges: RequestMemo::default(),
            resolutions: RequestMemo::default(),
            symbols_by_id: RequestMemo::default(),
            symbols_by_file: RequestMemo::default(),
            symbols_by_files: RequestMemo::default(),
            symbols_by_path_name: RequestMemo::default(),
            file_manifests: RequestMemo::default(),
            file_exists: RequestMemo::default(),
            temporal_index: OnceLock::new(),
            symbol_histories: RequestMemo::default(),
        }
    }

    fn unbounded_search_options(options: &SearchOptions) -> SearchOptions {
        let mut options = options.clone();
        options.limit = MAX_SEARCH_RESULTS;
        options
    }

    fn limited_search_result(mut result: SearchResult, requested_limit: usize) -> SearchResult {
        let limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        result.candidates.truncate(limit);
        result.truncated = result.total_matches > limit;
        result
    }
}

impl GraphQueryClient for RequestReplayClient<'_> {
    fn search_symbols(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        let unbounded = Self::unbounded_search_options(options);
        let result = self
            .searches
            .get_or_try_insert_with(unbounded.clone(), || self.base.search_symbols(&unbounded))?;
        Ok(Self::limited_search_result(result, options.limit))
    }

    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord> {
        self.caller_edges
            .get_or_insert_with(sid.to_owned(), || self.base.find_caller_edges(sid))
    }

    fn find_unresolved_caller_edges_by_labels(
        &self,
        target_labels: &HashSet<String>,
    ) -> Vec<OwnedCallerRecord> {
        let mut key = target_labels.iter().cloned().collect::<Vec<_>>();
        key.sort();
        self.unresolved_callers.get_or_insert_with(key, || {
            self.base
                .find_unresolved_caller_edges_by_labels(target_labels)
        })
    }

    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord> {
        self.callee_edges
            .get_or_insert_with(sid.to_owned(), || self.base.find_callee_edges(sid))
    }

    fn resolve_selector(&self, selector: &str) -> anyhow::Result<SelectorResolution> {
        self.resolutions
            .get_or_try_insert_with(selector.to_owned(), || self.base.resolve_selector(selector))
    }

    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        self.symbols_by_id
            .get_or_try_insert_with(sid.to_owned(), || self.base.symbol_by_id(sid))
    }

    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_by_file
            .get_or_try_insert_with(path.to_owned(), || self.base.symbols_by_file(path))
    }

    fn symbols_by_files(&self, paths: &[String]) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_by_files
            .get_or_try_insert_with(paths.to_vec(), || self.base.symbols_by_files(paths))
    }

    fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_by_path_name
            .get_or_try_insert_with((path.to_owned(), name.to_owned()), || {
                self.base.symbols_by_path_name(path, name)
            })
    }

    fn file_manifest_by_path(&self, path: &str) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        self.file_manifests
            .get_or_try_insert_with(path.to_owned(), || self.base.file_manifest_by_path(path))
    }

    fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        self.file_exists
            .get_or_try_insert_with(path.to_owned(), || self.base.file_exists(path))
    }

    fn temporal_index(&self) -> Arc<TemporalIndex> {
        Arc::clone(
            self.temporal_index
                .get_or_init(|| self.base.temporal_index()),
        )
    }

    fn symbol_history(
        &self,
        commits: &CommitIndexArtifact,
        symbol_id: &str,
    ) -> anyhow::Result<Vec<(GitSha, ChangeKind, SnapshotKey)>> {
        self.symbol_histories
            .get_or_try_insert_with((commits.clone(), symbol_id.to_owned()), || {
                self.base.symbol_history(commits, symbol_id)
            })
    }
}
