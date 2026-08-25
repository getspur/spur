use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::temporal::{GitSha, TemporalIndex};
use crate::{
    internal_unbounded_search_options, limited_search_result, ChangeKind, CommitIndexArtifact,
    GraphFileManifestEntry, GraphQueryClient, GraphSymbolArtifact, OwnedCalleeRecord,
    OwnedCallerRecord, SearchOptions, SearchResult, SelectorResolution, SnapshotKey,
};

type SymbolHistoryKey = (CommitIndexArtifact, String);
type SymbolHistory = Vec<(GitSha, ChangeKind, SnapshotKey)>;

struct RequestMemo<K, V> {
    entries: Mutex<Vec<(K, Arc<OnceLock<V>>)>>,
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
    fn entries(&self) -> MutexGuard<'_, Vec<(K, Arc<OnceLock<V>>)>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn entry(&self, key: K) -> Arc<OnceLock<V>> {
        let mut entries = self.entries();
        if let Some((_, entry)) = entries.iter().find(|(candidate, _)| candidate == &key) {
            return Arc::clone(entry);
        }
        let entry = Arc::new(OnceLock::new());
        entries.push((key, Arc::clone(&entry)));
        entry
    }

    fn get_or_insert_with(&self, key: K, load: impl FnOnce() -> V) -> V {
        self.entry(key).get_or_init(load).clone()
    }
}

type SharedRequestResult<V> = Result<V, SharedRequestError>;

#[derive(Clone)]
struct SharedRequestError(Arc<anyhow::Error>);

impl SharedRequestError {
    fn new(error: anyhow::Error) -> Self {
        Self(Arc::new(error))
    }
}

impl fmt::Debug for SharedRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for SharedRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for SharedRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl<K, V> RequestMemo<K, SharedRequestResult<V>>
where
    K: PartialEq,
    V: Clone,
{
    fn get_or_try_insert_with(
        &self,
        key: K,
        load: impl FnOnce() -> anyhow::Result<V>,
    ) -> anyhow::Result<V> {
        self.get_or_insert_with(key, || load().map_err(SharedRequestError::new))
            .map_err(anyhow::Error::new)
    }
}

/// Typed, request-local replay of base graph operations.
///
/// A request executes its handler once against this client before freshness
/// analysis. If a dirty overlay is required, the same client becomes the
/// overlay's base. Repeated operations are then served from these typed memos
/// rather than querying Parquet again. Fallible operations memoize a shared
/// result so both successes and contextual errors retain their source chains.
pub(super) struct RequestReplayClient<'a> {
    base: &'a (dyn GraphQueryClient + Sync),
    searches: RequestMemo<SearchOptions, SharedRequestResult<SearchResult>>,
    caller_edges: RequestMemo<String, Vec<OwnedCallerRecord>>,
    unresolved_callers: RequestMemo<Vec<String>, Vec<OwnedCallerRecord>>,
    callee_edges: RequestMemo<String, Vec<OwnedCalleeRecord>>,
    resolutions: RequestMemo<String, SharedRequestResult<SelectorResolution>>,
    symbols_by_id: RequestMemo<String, SharedRequestResult<Option<GraphSymbolArtifact>>>,
    symbols_by_file: RequestMemo<String, SharedRequestResult<Vec<GraphSymbolArtifact>>>,
    symbols_by_files: RequestMemo<Vec<String>, SharedRequestResult<Vec<GraphSymbolArtifact>>>,
    symbols_by_path_name:
        RequestMemo<(String, String), SharedRequestResult<Vec<GraphSymbolArtifact>>>,
    file_manifests: RequestMemo<String, SharedRequestResult<Option<GraphFileManifestEntry>>>,
    file_exists: RequestMemo<String, SharedRequestResult<bool>>,
    temporal_index: OnceLock<Arc<TemporalIndex>>,
    symbol_histories: RequestMemo<SymbolHistoryKey, SharedRequestResult<SymbolHistory>>,
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
}

impl GraphQueryClient for RequestReplayClient<'_> {
    fn search_symbols(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        let unbounded = internal_unbounded_search_options(options);
        let result = self
            .searches
            .get_or_try_insert_with(unbounded.clone(), || self.base.search_symbols(&unbounded))?;
        Ok(limited_search_result(
            result.candidates,
            result.total_matches,
            options.limit,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::Duration;

    struct SlowCountingClient {
        loads: AtomicUsize,
        fail: bool,
    }

    impl SlowCountingClient {
        fn new(fail: bool) -> Self {
            Self {
                loads: AtomicUsize::new(0),
                fail,
            }
        }
    }

    impl GraphQueryClient for SlowCountingClient {
        fn search_symbols(&self, _options: &SearchOptions) -> anyhow::Result<SearchResult> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            if self.fail {
                return Err(anyhow::anyhow!("inner replay error")).context("outer replay context");
            }
            Ok(SearchResult {
                candidates: Vec::new(),
                total_matches: 0,
                truncated: false,
            })
        }

        fn find_caller_edges(&self, _sid: &str) -> Vec<OwnedCallerRecord> {
            panic!("unused test operation")
        }

        fn find_callee_edges(&self, _sid: &str) -> Vec<OwnedCalleeRecord> {
            panic!("unused test operation")
        }

        fn resolve_selector(&self, _selector: &str) -> anyhow::Result<SelectorResolution> {
            panic!("unused test operation")
        }

        fn symbol_by_id(&self, _sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
            panic!("unused test operation")
        }

        fn symbols_by_file(&self, _path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
            panic!("unused test operation")
        }

        fn symbols_by_path_name(
            &self,
            _path: &str,
            _name: &str,
        ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
            panic!("unused test operation")
        }

        fn file_manifest_by_path(
            &self,
            _path: &str,
        ) -> anyhow::Result<Option<GraphFileManifestEntry>> {
            panic!("unused test operation")
        }

        fn file_exists(&self, _path: &str) -> anyhow::Result<bool> {
            panic!("unused test operation")
        }

        fn temporal_index(&self) -> Arc<TemporalIndex> {
            panic!("unused test operation")
        }
    }

    fn search_options() -> SearchOptions {
        SearchOptions {
            query: "singleflight".to_owned(),
            mode: crate::SearchMode::Exact,
            filters: crate::SearchFilters::default(),
            limit: 20,
        }
    }

    #[test]
    fn concurrent_same_key_searches_singleflight_through_one_request_client() {
        let base = SlowCountingClient::new(false);
        let replay = RequestReplayClient::new(&base);
        let options = search_options();
        let start = Arc::new(Barrier::new(3));

        let results = std::thread::scope(|scope| {
            let first_start = Arc::clone(&start);
            let first_replay = &replay;
            let first_options = &options;
            let first = scope.spawn(move || {
                first_start.wait();
                first_replay.search_symbols(first_options)
            });
            let second_start = Arc::clone(&start);
            let second_replay = &replay;
            let second_options = &options;
            let second = scope.spawn(move || {
                second_start.wait();
                second_replay.search_symbols(second_options)
            });
            start.wait();
            [
                first.join().expect("first replay thread"),
                second.join().expect("second replay thread"),
            ]
        });

        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(
            base.loads.load(Ordering::SeqCst),
            1,
            "same-key request-local calls must share one base operation"
        );
    }

    #[test]
    fn contextual_search_error_is_replayed_once_with_identical_chains() {
        let base = SlowCountingClient::new(true);
        let replay = RequestReplayClient::new(&base);
        let options = search_options();

        let first = replay
            .search_symbols(&options)
            .expect_err("first base search must fail");
        let second = replay
            .search_symbols(&options)
            .expect_err("second search must replay the failure");
        let chains =
            [first, second].map(|error| error.chain().map(ToString::to_string).collect::<Vec<_>>());

        eprintln!(
            "request replay error evidence base_count={} chains={chains:?}",
            base.loads.load(Ordering::SeqCst)
        );
        assert_eq!(
            chains[0],
            vec!["outer replay context", "inner replay error"]
        );
        assert_eq!(chains[1], chains[0]);
        assert_eq!(
            base.loads.load(Ordering::SeqCst),
            1,
            "a replayed error must not execute the base operation again"
        );
    }
}
