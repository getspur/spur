use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use spur_graph::git_blob_oid;
use spur_graph::temporal::{
    resolve_symbol_at, symbol_history, Resolution, ResolutionFailure, TemporalIndex,
};
use spur_graph::{
    bounded_subgraph_with_budget, edge_kind, find_callee_edges, find_caller_edges, load_artifact,
    resolve_artifact_location, resolve_selector, resolve_worktree_root_from, search_symbols,
    CalleeRecord, CallerRecord, CandidateRow, CommitIndexArtifact, GraphEdgeArtifact,
    GraphEdgeKind, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexPointer,
    GraphSymbolArtifact, SearchFilters, SearchMode, SearchOptions, SelectorResolution, SnapshotKey,
    SubgraphBudget, CODE_SYMBOL_URI_PREFIX,
};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

#[allow(dead_code)]
mod file_oid_cache {
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::fs::{self, Metadata};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    use super::current_file_oid;

    const FILE_OID_CACHE_CAPACITY: usize = 4096;

    static FILE_OID_CACHE: OnceLock<Mutex<FileOidCache>> = OnceLock::new();

    pub(super) fn file_oid_match(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        rel_path: &str,
        indexed_oid: &str,
    ) -> Option<bool> {
        file_oid_match_inner(
            worktree,
            worktree_head_oid,
            graph_content_hash,
            rel_path,
            indexed_oid,
            None,
        )
        .as_bool()
    }

    pub(super) fn file_oid_match_detail(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        rel_path: &str,
        indexed_oid: &str,
    ) -> FileOidMatch {
        file_oid_match_inner(
            worktree,
            worktree_head_oid,
            graph_content_hash,
            rel_path,
            indexed_oid,
            None,
        )
    }

    fn file_oid_match_inner(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        rel_path: &str,
        indexed_oid: &str,
        after_first_stat: Option<&dyn Fn(&Path)>,
    ) -> FileOidMatch {
        let path = worktree.join(rel_path);
        let Some(before) = FileMetadataKey::from_path(&path) else {
            return FileOidMatch::Unknown;
        };
        if let Some(after_first_stat) = after_first_stat {
            after_first_stat(&path);
        }
        let key = FileOidCacheKey {
            worktree_root: worktree.to_path_buf(),
            worktree_head_oid: worktree_head_oid.to_string(),
            graph_content_hash: graph_content_hash.to_string(),
            rel_path: rel_path.to_string(),
            metadata: before.clone(),
        };

        let Ok(mut cache_guard) = cache().lock() else {
            return FileOidMatch::Unknown;
        };
        if let Some(cached_oid) = cache_guard.get(&key) {
            let Some(after) = FileMetadataKey::from_path(&path) else {
                return FileOidMatch::Unknown;
            };
            if before != after {
                return FileOidMatch::Unknown;
            }
            return compare_file_oids(&cached_oid, indexed_oid);
        }
        drop(cache_guard);

        let Some(current_oid) = current_file_oid(worktree, rel_path).ok().flatten() else {
            return FileOidMatch::Unknown;
        };
        let Some(after) = FileMetadataKey::from_path(&path) else {
            return FileOidMatch::Unknown;
        };
        if before != after {
            return FileOidMatch::Unknown;
        }

        let Ok(mut cache_guard) = cache().lock() else {
            return FileOidMatch::Unknown;
        };
        cache_guard.insert(key, current_oid.clone());
        compare_file_oids(&current_oid, indexed_oid)
    }

    #[cfg(test)]
    pub(super) fn file_oid_match_after_first_stat(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        rel_path: &str,
        indexed_oid: &str,
        after_first_stat: &dyn Fn(&Path),
    ) -> Option<bool> {
        file_oid_match_inner(
            worktree,
            worktree_head_oid,
            graph_content_hash,
            rel_path,
            indexed_oid,
            Some(after_first_stat),
        )
        .as_bool()
    }

    pub(super) fn aggregate_file_oids_match(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        files: &[(&str, &str)],
    ) -> Option<bool> {
        aggregate_file_oid_report(worktree, worktree_head_oid, graph_content_hash, files).verdict
    }

    pub(super) fn aggregate_file_oid_report(
        worktree: &Path,
        worktree_head_oid: &str,
        graph_content_hash: &str,
        files: &[(&str, &str)],
    ) -> FileOidAggregateReport {
        let mut dirty_oids = BTreeMap::new();
        let mut saw_unknown = false;
        for (rel_path, indexed_oid) in files {
            match file_oid_match_detail(
                worktree,
                worktree_head_oid,
                graph_content_hash,
                rel_path,
                indexed_oid,
            ) {
                FileOidMatch::Match => {}
                FileOidMatch::Mismatch { current_oid } => {
                    dirty_oids.insert(PathBuf::from(*rel_path), current_oid);
                }
                FileOidMatch::Unknown => saw_unknown = true,
            }
        }

        let verdict = if !dirty_oids.is_empty() {
            Some(false)
        } else if saw_unknown {
            None
        } else {
            Some(true)
        };

        FileOidAggregateReport {
            verdict,
            dirty_oids,
        }
    }

    fn cache() -> &'static Mutex<FileOidCache> {
        FILE_OID_CACHE.get_or_init(|| Mutex::new(FileOidCache::new(FILE_OID_CACHE_CAPACITY)))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum FileOidMatch {
        Match,
        Mismatch { current_oid: [u8; 20] },
        Unknown,
    }

    impl FileOidMatch {
        fn as_bool(&self) -> Option<bool> {
            match self {
                Self::Match => Some(true),
                Self::Mismatch { .. } => Some(false),
                Self::Unknown => None,
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct FileOidAggregateReport {
        pub verdict: Option<bool>,
        pub dirty_oids: BTreeMap<PathBuf, [u8; 20]>,
    }

    fn compare_file_oids(current_oid: &str, indexed_oid: &str) -> FileOidMatch {
        if current_oid == indexed_oid {
            return FileOidMatch::Match;
        }

        match parse_git_oid(current_oid) {
            Some(current_oid) => FileOidMatch::Mismatch { current_oid },
            None => FileOidMatch::Unknown,
        }
    }

    fn parse_git_oid(oid: &str) -> Option<[u8; 20]> {
        if oid.len() != 40 {
            return None;
        }
        let mut bytes = [0_u8; 20];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            *byte = u8::from_str_radix(&oid[start..start + 2], 16).ok()?;
        }
        Some(bytes)
    }

    #[derive(Debug)]
    struct FileOidCache {
        entries: HashMap<FileOidCacheKey, String>,
        lru: VecDeque<FileOidCacheKey>,
        capacity: usize,
    }

    impl FileOidCache {
        fn new(capacity: usize) -> Self {
            Self {
                entries: HashMap::new(),
                lru: VecDeque::new(),
                capacity,
            }
        }

        fn get(&mut self, key: &FileOidCacheKey) -> Option<String> {
            let value = self.entries.get(key).cloned()?;
            self.touch(key);
            Some(value)
        }

        fn insert(&mut self, key: FileOidCacheKey, value: String) {
            self.entries.insert(key.clone(), value);
            self.touch(&key);
            while self.entries.len() > self.capacity {
                let Some(expired) = self.lru.pop_front() else {
                    break;
                };
                self.entries.remove(&expired);
            }
        }

        fn touch(&mut self, key: &FileOidCacheKey) {
            self.lru.retain(|candidate| candidate != key);
            self.lru.push_back(key.clone());
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct FileOidCacheKey {
        worktree_root: PathBuf,
        worktree_head_oid: String,
        graph_content_hash: String,
        rel_path: String,
        metadata: FileMetadataKey,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct FileMetadataKey {
        dev: u64,
        ino: u64,
        size: u64,
        mtime_ns: i128,
    }

    impl FileMetadataKey {
        fn from_path(path: &Path) -> Option<Self> {
            let metadata = fs::symlink_metadata(path).ok()?;
            Some(Self::from_metadata(&metadata))
        }
    }

    #[cfg(unix)]
    impl FileMetadataKey {
        fn from_metadata(metadata: &Metadata) -> Self {
            use std::os::unix::fs::MetadataExt;

            let mtime_ns = i128::from(metadata.mtime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(metadata.mtime_nsec()));

            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                size: metadata.size(),
                mtime_ns,
            }
        }
    }

    #[cfg(not(unix))]
    impl FileMetadataKey {
        fn from_metadata(metadata: &Metadata) -> Self {
            let mtime_ns = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos() as i128)
                .unwrap_or_default();

            Self {
                dev: 0,
                ino: 0,
                size: metadata.len(),
                mtime_ns,
            }
        }
    }
}

#[allow(dead_code)]
#[path = "rebuild_singleflight.rs"]
mod rebuild_singleflight;
pub(crate) use rebuild_singleflight::RebuildCoordinator;
use rebuild_singleflight::RebuildKey;

const MAX_MCP_CODE_SUBGRAPH_RADIUS: u8 = 3;
const DEFAULT_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 40;
const MIN_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 1;
const MAX_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 400;
const DEFAULT_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 120;
const MIN_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 1;
const MAX_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 1200;
const MAX_MCP_CODE_READ_SYMBOL_CONTEXT_LINES: usize = 50;
const GRAPH_POINTER_RELATIVE_PATH: &str = ".spur/graph-index.pointer.json";
const GRAPH_GIT_METADATA_TIMEOUT: Duration = Duration::from_millis(200);
const DEFAULT_GRAPH_REBUILD_LATENCY_BUDGET: Duration = Duration::from_millis(750);
#[cfg(any(test, feature = "test-support"))]
const GRAPH_REBUILD_LATENCY_BUDGET_UNSET_MS: u64 = u64::MAX;
#[cfg(any(test, feature = "test-support"))]
static GRAPH_REBUILD_LATENCY_BUDGET_OVERRIDE_MS: AtomicU64 =
    AtomicU64::new(GRAPH_REBUILD_LATENCY_BUDGET_UNSET_MS);
#[cfg(any(test, feature = "test-support"))]
static GRAPH_REBUILD_DELAY_MS: AtomicU64 = AtomicU64::new(0);
// Temporal resolution error codes (T3 / Phase 1.5 hardening)
const CODE_GRAPH_NOT_FOUND_ERROR_CODE: i64 = -32004;
const CODE_GRAPH_DELETED_ERROR_CODE: i64 = -32005;
const CODE_GRAPH_AMBIGUOUS_ERROR_CODE: i64 = -32006;
const CODE_GRAPH_UNKNOWN_ERROR_CODE: i64 = -32007;

impl McpCallbackServer {
    pub(crate) async fn handle_code_resolve(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_resolve_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_search(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_search_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_file_symbols(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_file_symbols_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_symbol_info(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_symbol_info_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_read_symbol(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_read_symbol_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_callers(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_callers_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_callees(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_callees_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_subgraph_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }

    pub(crate) async fn handle_code_symbol_history(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        code_graph_response(
            id,
            code_symbol_history_response(&args, Arc::clone(&self.graph_rebuild_coordinator)).await,
        )
        .await
    }
}

pub(crate) async fn code_resolve(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body =
        code_resolve_with_artifact(args, &artifact).map_err(CodeGraphError::into_handler_error)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

pub(crate) async fn code_search(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_search_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_search_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_search_with_artifact(args, artifact).map_err(CodeGraphError::from)
    })
    .await
}

fn code_search_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let request = code_search_options(args)?;
    let options = request.options;
    let result = search_symbols(artifact, &options);
    let candidates = result
        .candidates
        .into_iter()
        .map(candidate_row_for_symbol)
        .map(candidate_row)
        .collect::<Vec<_>>();

    let mut body = json!({
        "query": options.query,
        "mode": search_mode_str(options.mode),
        "symbol_kind": options.filters.symbol_kind,
        "file": options.filters.file,
        "file_glob": options.filters.file_glob,
        "limit": options.limit,
        "total_matches": result.total_matches,
        "truncated": result.truncated,
        "candidates": candidates,
    });
    if let Some(requested_limit) = request.requested_limit {
        body["requested_limit"] = requested_limit;
    }
    Ok(body)
}

async fn code_resolve_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_resolve_with_artifact(args, artifact)
    })
    .await
}

fn code_resolve_with_artifact(args: &Value, artifact: &GraphIndexArtifact) -> CodeGraphResult {
    let selector = selector_arg(args)?;
    let Some(as_of) = parse_as_of(args)? else {
        let candidates = resolve_candidate_rows(artifact, selector)?
            .into_iter()
            .map(candidate_row)
            .collect::<Vec<_>>();

        return Ok(json!({ "candidates": candidates }));
    };

    let resolved = match resolve_selector(artifact, selector) {
        SelectorResolution::Resolved(resolved) => resolved,
        SelectorResolution::Ambiguous { candidates } => {
            let candidates = candidates
                .into_iter()
                .map(candidate_row)
                .collect::<Vec<_>>();
            return Ok(json!({ "candidates": candidates }));
        }
        SelectorResolution::NotFound => {
            return Err(McpHandlerError::NotFound(format!(
                "symbol {} not found in graph artifact",
                missing_symbol_label(selector)
            ))
            .into());
        }
    };

    let worktree = current_worktree()?;
    let commits = load_commit_index_for_request(&worktree)?;
    let resolution = resolve_symbol_as_of(artifact, &commits, &resolved.stable_symbol_id, &as_of)?;
    code_resolve_temporal_response(artifact, &resolved.stable_symbol_id, &as_of, resolution)
}

fn code_resolve_temporal_response(
    artifact: &GraphIndexArtifact,
    requested_symbol_id: &str,
    as_of: &str,
    resolution: Resolution<String>,
) -> CodeGraphResult {
    match resolution {
        Resolution::Found { value, chain } => {
            let symbol = symbol_by_id(artifact, &value)?;
            let kind = if value == requested_symbol_id {
                "found"
            } else {
                "renamed"
            };
            Ok(json!({
                "candidates": [candidate_row(candidate_row_for_symbol(symbol))],
                "resolution": {
                    "kind": kind,
                    "as_of": as_of,
                    "symbol": symbol_uri(&value),
                    "chain": chain,
                },
            }))
        }
        Resolution::Deleted { last_seen } => Ok(json!({
            "candidates": [],
            "resolution": {
                "kind": "deleted",
                "as_of": as_of,
                "last_seen": last_seen,
            },
        })),
        Resolution::Ambiguous { candidates } => {
            let candidate_rows = candidates
                .iter()
                .map(|candidate| symbol_by_id(artifact, candidate).map(candidate_row_for_symbol))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(candidate_row)
                .collect::<Vec<_>>();
            Ok(json!({
                "candidates": candidate_rows,
                "resolution": {
                    "kind": "ambiguous",
                    "as_of": as_of,
                    "candidates": candidates,
                },
            }))
        }
        Resolution::Unknown { reason } => {
            Err(unknown_resolution_error(requested_symbol_id, as_of, reason))
        }
    }
}

pub(crate) async fn code_file_symbols(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_file_symbols_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_file_symbols_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_file_symbols_with_artifact(args, artifact).map_err(CodeGraphError::from)
    })
    .await
}

fn code_file_symbols_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let file = file_arg(args)?;
    let file = validate_file_path_arg(file)?;
    if !artifact.files.iter().any(|entry| entry.file_path == file) {
        return Err(McpHandlerError::NotFound(format!(
            "file `{file}` not found in graph artifact"
        )));
    }

    let symbols = candidate_rows_for_symbols(
        artifact
            .symbols
            .iter()
            .filter(|symbol| symbol.file_path == file),
    )
    .into_iter()
    .map(candidate_row)
    .collect::<Vec<_>>();

    Ok(json!({ "symbols": symbols }))
}

pub(crate) async fn code_symbol_info(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_symbol_info_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_symbol_info_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_symbol_info_with_artifact(args, artifact).map_err(CodeGraphError::from)
    })
    .await
}

fn code_symbol_info_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };
    let symbol = symbol_by_id(artifact, &symbol_id)?;

    Ok(json!({ "symbol": symbol_info_row(symbol) }))
}

pub(crate) async fn code_read_symbol(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_read_symbol_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_read_symbol_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_read_symbol_with_artifact(args, artifact).map_err(CodeGraphError::from)
    })
    .await
}

fn code_read_symbol_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol = match code_read_symbol_target(args, artifact)? {
        CodeReadSymbolTarget::Resolved(symbol) => symbol,
        CodeReadSymbolTarget::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };
    let context_lines = clamped_usize_arg(
        args,
        "context_lines",
        0,
        0,
        MAX_MCP_CODE_READ_SYMBOL_CONTEXT_LINES,
    )?;
    let manifest = file_manifest_for_symbol(artifact, symbol)?;
    let worktree = current_worktree_root().ok_or_else(|| {
        McpHandlerError::Internal("failed to resolve current worktree root".into())
    })?;
    let indexed_bytes =
        read_indexed_file_bytes(&worktree, &symbol.file_path, &manifest.content_oid)?;
    let indexed_source = String::from_utf8(indexed_bytes).map_err(|error| {
        McpHandlerError::Internal(format!(
            "indexed blob `{}` for `{}` is not UTF-8: {error}",
            manifest.content_oid, symbol.file_path
        ))
    })?;
    let source_range = source_range_with_context(&indexed_source, symbol, context_lines.value);
    let source = source_for_line_range(&indexed_source, source_range);
    let current_oid = current_file_oid(&worktree, &symbol.file_path)?;
    let stale = current_oid.as_deref() != Some(manifest.content_oid.as_str());

    let mut body = json!({
        "symbol": symbol_info_row(symbol),
        "source": source,
        "line_range": {
            "start": source_range[0],
            "end": source_range[1],
        },
        "file_oid": manifest.content_oid,
        "context_lines": context_lines.value,
    });
    if let Some(requested_context_lines) = context_lines.requested_value {
        body["requested_context_lines"] = requested_context_lines;
    }
    if stale {
        body["stale"] = Value::Bool(true);
    }
    Ok(body)
}

pub(crate) async fn code_callers(args: &Value) -> Result<Value, McpHandlerError> {
    selected_code_selector(args)?;
    let artifact = load_graph_artifact_for_request()?;
    let body =
        code_callers_with_artifact(args, &artifact).map_err(CodeGraphError::into_handler_error)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_callers_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_callers_with_artifact(args, artifact)
    })
    .await
}

fn code_callers_with_artifact(args: &Value, artifact: &GraphIndexArtifact) -> CodeGraphResult {
    let request = code_traversal_request(args)?;
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };
    let symbol_id = resolve_symbol_for_optional_as_of_current_worktree(artifact, &symbol_id, args)?;

    let records = find_caller_edges(artifact, &symbol_id);
    let summary = caller_summary(&records);
    let callers = records
        .into_iter()
        .filter(|record| request.include_unresolved || record.is_resolved())
        .map(caller_row)
        .collect::<Vec<_>>();
    Ok(json!({
        "callers": callers,
        "include_unresolved": request.include_unresolved,
        "counts_by_kind": summary.counts_by_kind,
        "unresolved_sample": summary.unresolved_sample,
    }))
}

pub(crate) async fn code_callees(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body =
        code_callees_with_artifact(args, &artifact).map_err(CodeGraphError::into_handler_error)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_callees_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_callees_with_artifact(args, artifact)
    })
    .await
}

fn code_callees_with_artifact(args: &Value, artifact: &GraphIndexArtifact) -> CodeGraphResult {
    let request = code_traversal_request(args)?;
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };
    let symbol_id = resolve_symbol_for_optional_as_of_current_worktree(artifact, &symbol_id, args)?;

    let records = find_callee_edges(artifact, &symbol_id);
    let summary = callee_summary(&records);
    let callees = records
        .into_iter()
        .filter(|record| request.include_unresolved || record.is_resolved())
        .map(callee_row)
        .collect::<Vec<_>>();
    Ok(json!({
        "callees": callees,
        "include_unresolved": request.include_unresolved,
        "counts_by_kind": summary.counts_by_kind,
        "unresolved_sample": summary.unresolved_sample,
    }))
}

pub(crate) async fn code_subgraph(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body =
        code_subgraph_with_artifact(args, &artifact).map_err(CodeGraphError::into_handler_error)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_subgraph_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    with_loaded_graph_artifact(Some(rebuild_coordinator), |artifact| {
        code_subgraph_with_artifact(args, artifact)
    })
    .await
}

fn code_subgraph_with_artifact(args: &Value, artifact: &GraphIndexArtifact) -> CodeGraphResult {
    let request = code_traversal_request(args)?;
    let root_ids = match code_subgraph_root_ids(args, artifact)? {
        CodeSubgraphRoots::RootIds(root_ids) => root_ids,
        CodeSubgraphRoots::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };

    let requested_radius = args
        .get("radius")
        .and_then(|value| value.as_u64())
        .unwrap_or(1);
    let radius = requested_radius.min(u64::from(MAX_MCP_CODE_SUBGRAPH_RADIUS)) as u8;
    let warning = (requested_radius > u64::from(MAX_MCP_CODE_SUBGRAPH_RADIUS)).then(|| {
        format!(
            "radius {requested_radius} exceeds max {MAX_MCP_CODE_SUBGRAPH_RADIUS}; clamped to {MAX_MCP_CODE_SUBGRAPH_RADIUS}"
        )
    });
    let format = args
        .get("format")
        .and_then(|value| value.as_str())
        .unwrap_or("json");
    let edge_kinds = parse_edge_kinds(args)?;
    let edge_filter = edge_kinds.as_deref();
    let budget = code_subgraph_budget(args)?;
    let root_refs = root_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let view = bounded_subgraph_with_budget(
        artifact,
        &root_refs,
        radius,
        edge_filter,
        request.include_unresolved,
        budget.budget,
    );

    match format {
        "json" => {
            let mut metadata = code_subgraph_metadata(radius, view.truncated, &budget);
            if let Some(warning) = warning {
                metadata["warning"] = Value::String(warning);
            }
            Ok(json!({
                "nodes": view.nodes.into_iter().map(symbol_row).collect::<Vec<_>>(),
                "edges": view.edges.into_iter().map(edge_row).collect::<Vec<_>>(),
                "truncated_frontier": view.truncated_frontier,
                "include_unresolved": request.include_unresolved,
                "metadata": metadata,
            }))
        }
        "mermaid" => {
            let mut metadata = code_subgraph_metadata(radius, view.truncated, &budget);
            if let Some(warning) = warning {
                metadata["warning"] = Value::String(warning);
            }
            let mermaid = mermaid_subgraph(&view.nodes, &view.edges);
            Ok(json!({
                "mermaid": mermaid,
                "truncated_frontier": view.truncated_frontier,
                "include_unresolved": request.include_unresolved,
                "metadata": metadata,
            }))
        }
        other => Err(McpHandlerError::InvalidParams(format!(
            "invalid format `{other}`; expected `json` or `mermaid`"
        ))
        .into()),
    }
}

pub(crate) async fn code_symbol_history(args: &Value) -> Result<Value, McpHandlerError> {
    let worktree = current_worktree()?;
    let artifact = load_graph_artifact_for_worktree(&worktree)?;
    let commits = load_commit_index_for_request(&worktree)?;
    let symbol_id = symbol_id_arg(args)?;
    let events = code_symbol_history_events(args, &artifact, &commits, &symbol_id)?;
    let files = response_file_set_for_symbol_ids(&artifact, [symbol_id.as_str()]);
    Ok(with_graph_metadata_with_files(
        &artifact,
        json!({
            "symbol": symbol_uri(&symbol_id),
            "events": events,
        }),
        &files,
    )
    .await)
}

async fn code_symbol_history_response(
    args: &Value,
    rebuild_coordinator: Arc<RebuildCoordinator>,
) -> CodeGraphResult {
    let worktree = current_worktree()?;
    let commits =
        load_commit_index_for_request(&worktree).map_err(CodeGraphError::without_metadata)?;
    let symbol_id = symbol_id_arg(args)
        .map_err(CodeGraphError::without_metadata)?
        .to_string();

    with_loaded_graph_artifact_for_worktree(&worktree, Some(rebuild_coordinator), |artifact| {
        let events = code_symbol_history_events(args, artifact, &commits, &symbol_id)?;
        let files = response_file_set_for_symbol_ids(artifact, [symbol_id.as_str()]);
        Ok(GraphResponsePayload::with_files(
            json!({
                "symbol": symbol_uri(&symbol_id),
                "events": events,
            }),
            files,
        ))
    })
    .await
}

fn code_symbol_history_events(
    args: &Value,
    artifact: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol_id: &str,
) -> Result<Vec<Value>, McpHandlerError> {
    let index = TemporalIndex::new(artifact);
    let reachable = parse_as_of(args)?
        .map(|as_of| reachable_commits(commits, &as_of))
        .transpose()?;
    if artifact.symbol_snapshots.is_empty() {
        return Ok(Vec::new());
    }

    Ok(symbol_history(&index, commits, symbol_id)
        .into_iter()
        .filter(|(sha, _, _)| {
            reachable
                .as_ref()
                .is_none_or(|reachable| reachable.contains(sha))
        })
        .map(|(sha, change_kind, key)| {
            json!({
                "commit": sha,
                "change_kind": change_kind,
                "snapshot": key,
            })
        })
        .collect::<Vec<_>>())
}

type CodeGraphResult = Result<Value, CodeGraphError>;
type CodeGraphPayloadResult = Result<GraphResponsePayload, CodeGraphError>;

#[derive(Debug)]
struct GraphResponsePayload {
    body: Value,
    files: Option<Vec<(String, String)>>,
}

impl GraphResponsePayload {
    fn body(body: Value) -> Self {
        Self { body, files: None }
    }

    fn with_files(body: Value, files: Vec<(String, String)>) -> Self {
        Self {
            body,
            files: Some(files),
        }
    }

    fn files_for_metadata(&self, artifact: &GraphIndexArtifact) -> Vec<(String, String)> {
        self.files
            .clone()
            .or_else(|| empty_code_search_file_set(artifact, &self.body))
            .unwrap_or_else(|| response_file_set_from_body(artifact, &self.body))
    }
}

#[derive(Debug)]
struct CodeGraphError {
    error: McpHandlerError,
    metadata: Option<Box<GraphMetadataSource>>,
    /// For temporal resolution failures, carries the JSON-RPC error code (e.g. -32005 for Deleted).
    temporal_code: Option<i64>,
    /// For temporal resolution failures, carries the structured error data payload.
    temporal_data: Option<Box<Value>>,
}

impl CodeGraphError {
    fn without_metadata(error: McpHandlerError) -> Self {
        Self {
            error,
            metadata: None,
            temporal_code: None,
            temporal_data: None,
        }
    }

    fn with_temporal(code: i64, message: String, data: Value) -> Self {
        Self {
            error: McpHandlerError::Internal(message),
            metadata: None,
            temporal_code: Some(code),
            temporal_data: Some(Box::new(data)),
        }
    }

    fn with_artifact_metadata(mut self, artifact: &GraphIndexArtifact) -> Self {
        if self.metadata.is_none() && self.temporal_code.is_none() {
            self.metadata = Some(Box::new(GraphMetadataSource::from_artifact(artifact)));
        }
        self
    }

    fn into_handler_error(self) -> McpHandlerError {
        self.error
    }
}

fn resolution_error(code: i64, message: String, data: Value) -> CodeGraphError {
    CodeGraphError::with_temporal(code, message, data)
}

impl From<McpHandlerError> for CodeGraphError {
    fn from(error: McpHandlerError) -> Self {
        Self::without_metadata(error)
    }
}

fn deleted_resolution_error(
    symbol_id: &str,
    as_of: &str,
    last_seen: SnapshotKey,
) -> CodeGraphError {
    resolution_error(
        CODE_GRAPH_DELETED_ERROR_CODE,
        format!("symbol {symbol_id} was deleted at or before commit `{as_of}`"),
        json!({
            "kind": "deleted",
            "last_seen": last_seen,
        }),
    )
}

fn ambiguous_resolution_error(
    symbol_id: &str,
    as_of: &str,
    candidates: Vec<String>,
) -> CodeGraphError {
    resolution_error(
        CODE_GRAPH_AMBIGUOUS_ERROR_CODE,
        format!(
            "symbol {symbol_id} is ambiguous at commit `{as_of}`; candidates: {}",
            candidates.join(", ")
        ),
        json!({
            "kind": "ambiguous",
            "candidates": candidates,
        }),
    )
}

fn unknown_resolution_error(
    symbol_id: &str,
    as_of: &str,
    reason: ResolutionFailure,
) -> CodeGraphError {
    let reason_message = format_resolution_failure(&reason);
    resolution_error(
        CODE_GRAPH_UNKNOWN_ERROR_CODE,
        format!("symbol {symbol_id} could not be resolved at commit `{as_of}` ({reason_message})"),
        json!({
            "kind": "unknown",
            "reason": resolution_failure_data(&reason),
        }),
    )
}

fn resolution_failure_data(reason: &ResolutionFailure) -> Value {
    match reason {
        ResolutionFailure::AnchorCommitNotIndexed(commit) => json!({
            "kind": "anchor_commit_not_indexed",
            "commit": commit,
        }),
        ResolutionFailure::SymbolNotPresentAtAnchor => json!({
            "kind": "symbol_not_present_at_anchor",
        }),
        ResolutionFailure::IndexCorrupt(message) => json!({
            "kind": "index_corrupt",
            "message": message,
        }),
    }
}

#[derive(Debug, Clone)]
struct GraphMetadataSource {
    graph_content_hash: String,
    graph_index_version: String,
    manifest_version: String,
}

impl GraphMetadataSource {
    fn from_artifact(artifact: &GraphIndexArtifact) -> Self {
        Self {
            graph_content_hash: artifact.graph_content_hash.clone(),
            graph_index_version: artifact.header.graph_index_version.clone(),
            manifest_version: artifact.manifest_version.clone(),
        }
    }
}

#[derive(Debug)]
struct GraphResponseMetadata {
    source: GraphMetadataSource,
    graph_built_at: Option<String>,
    indexed_head_oid: Option<String>,
    worktree_head_oid: Option<String>,
    worktree_dirty: Option<bool>,
    response_file_oids_match: Option<bool>,
    rebuild_status: RebuildStatus,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "snake_case")]
enum RebuildStatus {
    #[default]
    NotNeeded,
    Fresh,
    StaleBudgetExceeded,
    StaleRebuildFailed,
}

#[derive(Debug)]
struct GraphResponseAnalysis {
    metadata: GraphResponseMetadata,
    rebuild_candidate: Option<RebuildCandidate>,
}

#[derive(Debug)]
struct RebuildCandidate {
    worktree: PathBuf,
    key: RebuildKey,
}

impl GraphResponseMetadata {
    async fn from_artifact_with_files(
        artifact: &GraphIndexArtifact,
        files: &[(String, String)],
    ) -> Self {
        Self::analyze_artifact_with_files(artifact, files)
            .await
            .metadata
    }

    async fn from_source(source: GraphMetadataSource) -> Self {
        Self::from_source_inner(source, None).await
    }

    async fn analyze_artifact_with_files(
        artifact: &GraphIndexArtifact,
        files: &[(String, String)],
    ) -> GraphResponseAnalysis {
        Self::analyze_source_inner(GraphMetadataSource::from_artifact(artifact), Some(files)).await
    }

    async fn from_source_inner(
        source: GraphMetadataSource,
        response_files: Option<&[(String, String)]>,
    ) -> Self {
        Self::analyze_source_inner(source, response_files)
            .await
            .metadata
    }

    async fn analyze_source_inner(
        source: GraphMetadataSource,
        response_files: Option<&[(String, String)]>,
    ) -> GraphResponseAnalysis {
        let worktree = current_worktree_root();
        let pointer = worktree
            .as_deref()
            .and_then(|worktree| matching_graph_pointer(worktree, &source));
        let graph_built_at = pointer.as_ref().and_then(graph_built_at_from_pointer);
        let indexed_head_oid = pointer
            .as_ref()
            .and_then(|pointer| non_empty_string(pointer.indexed_commit_oid.clone()));
        let git = match worktree.as_deref() {
            Some(worktree) => worktree_git_metadata(worktree).await,
            None => None,
        };
        let worktree_head_oid = git.as_ref().map(|git| git.head_oid.clone());
        let worktree_dirty = git.as_ref().and_then(|git| {
            compute_worktree_dirty(
                indexed_head_oid.as_deref(),
                &git.head_oid,
                git.has_uncommitted_changes,
            )
        });
        let mut dirty_oids = BTreeMap::new();
        let response_file_oids_match = match (
            response_files,
            worktree.as_deref(),
            worktree_head_oid.as_deref(),
        ) {
            (Some(files), Some(worktree), Some(worktree_head_oid)) => {
                let files = files
                    .iter()
                    .map(|(rel_path, indexed_oid)| (rel_path.as_str(), indexed_oid.as_str()))
                    .collect::<Vec<_>>();
                let report = file_oid_cache::aggregate_file_oid_report(
                    worktree,
                    worktree_head_oid,
                    &source.graph_content_hash,
                    &files,
                );
                dirty_oids = report.dirty_oids;
                report.verdict
            }
            _ => None,
        };
        let rebuild_candidate = match (
            response_file_oids_match,
            worktree.as_ref(),
            worktree_head_oid.as_deref(),
        ) {
            (Some(false), Some(worktree), Some(head_oid)) if !dirty_oids.is_empty() => {
                Some(RebuildCandidate {
                    worktree: worktree.clone(),
                    key: RebuildKey::from(head_oid, &dirty_oids),
                })
            }
            _ => None,
        };

        GraphResponseAnalysis {
            metadata: Self {
                source,
                graph_built_at,
                indexed_head_oid,
                worktree_head_oid,
                worktree_dirty,
                response_file_oids_match,
                rebuild_status: RebuildStatus::NotNeeded,
            },
            rebuild_candidate,
        }
    }

    fn with_rebuild_status(mut self, rebuild_status: RebuildStatus) -> Self {
        self.rebuild_status = rebuild_status;
        match rebuild_status {
            RebuildStatus::Fresh => {
                self.worktree_dirty = Some(false);
                if self.indexed_head_oid.is_none() {
                    self.indexed_head_oid = self.worktree_head_oid.clone();
                }
            }
            RebuildStatus::StaleBudgetExceeded | RebuildStatus::StaleRebuildFailed => {
                self.worktree_dirty = Some(true);
            }
            RebuildStatus::NotNeeded => {}
        }
        self
    }

    fn into_value(self) -> Value {
        json!({
            "graph_content_hash": self.source.graph_content_hash,
            "graph_index_version": self.source.graph_index_version,
            "graph_built_at": self.graph_built_at,
            "indexed_head_oid": self.indexed_head_oid,
            "worktree_head_oid": self.worktree_head_oid,
            "worktree_dirty": self.worktree_dirty,
            "response_file_oids_match": self.response_file_oids_match,
            "rebuild_status": self.rebuild_status,
        })
    }

    fn insert_into(self, body: &mut Value) {
        if let Value::Object(map) = body {
            let Value::Object(metadata) = self.into_value() else {
                return;
            };
            map.extend(metadata);
        }
    }
}

async fn with_loaded_graph_artifact(
    rebuild_coordinator: Option<Arc<RebuildCoordinator>>,
    handler: impl Fn(&GraphIndexArtifact) -> CodeGraphResult,
) -> CodeGraphResult {
    let artifact =
        Arc::new(load_graph_artifact_for_request().map_err(CodeGraphError::without_metadata)?);
    with_loaded_graph_payload(rebuild_coordinator, artifact, |artifact| {
        handler(artifact).map(GraphResponsePayload::body)
    })
    .await
}

async fn with_loaded_graph_artifact_for_worktree(
    worktree: &Path,
    rebuild_coordinator: Option<Arc<RebuildCoordinator>>,
    handler: impl Fn(&GraphIndexArtifact) -> CodeGraphPayloadResult,
) -> CodeGraphResult {
    let artifact = Arc::new(
        load_graph_artifact_for_worktree(worktree).map_err(CodeGraphError::without_metadata)?,
    );
    with_loaded_graph_payload(rebuild_coordinator, artifact, handler).await
}

async fn with_loaded_graph_payload(
    rebuild_coordinator: Option<Arc<RebuildCoordinator>>,
    artifact: Arc<GraphIndexArtifact>,
    handler: impl Fn(&GraphIndexArtifact) -> CodeGraphPayloadResult,
) -> CodeGraphResult {
    let payload = handler(&artifact).map_err(|error| error.with_artifact_metadata(&artifact))?;
    Ok(with_graph_metadata_for_payload(rebuild_coordinator, artifact, payload, &handler).await)
}

async fn with_graph_metadata_for_payload(
    rebuild_coordinator: Option<Arc<RebuildCoordinator>>,
    artifact: Arc<GraphIndexArtifact>,
    mut payload: GraphResponsePayload,
    handler: &impl Fn(&GraphIndexArtifact) -> CodeGraphPayloadResult,
) -> Value {
    let files = payload.files_for_metadata(&artifact);
    let mut analysis = GraphResponseMetadata::analyze_artifact_with_files(&artifact, &files).await;

    if let (Some(rebuild_coordinator), Some(rebuild_candidate)) =
        (rebuild_coordinator, analysis.rebuild_candidate.take())
    {
        match try_rebuild_artifact(
            rebuild_coordinator,
            Arc::clone(&artifact),
            rebuild_candidate,
        )
        .await
        {
            RebuildAttempt::Fresh(rebuilt_artifact) => match handler(&rebuilt_artifact) {
                Ok(mut fresh_payload) => {
                    let fresh_files = fresh_payload.files_for_metadata(&rebuilt_artifact);
                    GraphResponseMetadata::analyze_artifact_with_files(
                        &rebuilt_artifact,
                        &fresh_files,
                    )
                    .await
                    .metadata
                    .with_rebuild_status(RebuildStatus::Fresh)
                    .insert_into(&mut fresh_payload.body);
                    return fresh_payload.body;
                }
                Err(error) => {
                    tracing::warn!(
                        target: "spur_mcp::path_a",
                        error = ?error,
                        "rebuilt code graph response failed; serving stale response"
                    );
                    analysis.metadata = analysis
                        .metadata
                        .with_rebuild_status(RebuildStatus::StaleRebuildFailed);
                }
            },
            RebuildAttempt::StaleBudgetExceeded => {
                analysis.metadata = analysis
                    .metadata
                    .with_rebuild_status(RebuildStatus::StaleBudgetExceeded);
            }
            RebuildAttempt::StaleRebuildFailed => {
                analysis.metadata = analysis
                    .metadata
                    .with_rebuild_status(RebuildStatus::StaleRebuildFailed);
            }
        }
    }

    analysis.metadata.insert_into(&mut payload.body);
    payload.body
}

enum RebuildAttempt {
    Fresh(Arc<GraphIndexArtifact>),
    StaleBudgetExceeded,
    StaleRebuildFailed,
}

fn graph_rebuild_latency_budget() -> Duration {
    #[cfg(any(test, feature = "test-support"))]
    {
        let override_ms = GRAPH_REBUILD_LATENCY_BUDGET_OVERRIDE_MS.load(Ordering::SeqCst);
        if override_ms != GRAPH_REBUILD_LATENCY_BUDGET_UNSET_MS {
            return Duration::from_millis(override_ms);
        }
    }
    DEFAULT_GRAPH_REBUILD_LATENCY_BUDGET
}

#[cfg(any(test, feature = "test-support"))]
fn duration_millis_for_test(duration: Duration) -> u64 {
    duration
        .as_millis()
        .min(u128::from(GRAPH_REBUILD_LATENCY_BUDGET_UNSET_MS - 1)) as u64
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) struct GraphRebuildBudgetGuard {
    previous_ms: u64,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for GraphRebuildBudgetGuard {
    fn drop(&mut self) {
        GRAPH_REBUILD_LATENCY_BUDGET_OVERRIDE_MS.store(self.previous_ms, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn set_graph_rebuild_latency_budget_for_test(
    budget: Duration,
) -> GraphRebuildBudgetGuard {
    let previous_ms = GRAPH_REBUILD_LATENCY_BUDGET_OVERRIDE_MS
        .swap(duration_millis_for_test(budget), Ordering::SeqCst);
    GraphRebuildBudgetGuard { previous_ms }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) struct GraphRebuildDelayGuard {
    previous_ms: u64,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for GraphRebuildDelayGuard {
    fn drop(&mut self) {
        GRAPH_REBUILD_DELAY_MS.store(self.previous_ms, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn set_graph_rebuild_delay_for_test(delay: Duration) -> GraphRebuildDelayGuard {
    let previous_ms =
        GRAPH_REBUILD_DELAY_MS.swap(duration_millis_for_test(delay), Ordering::SeqCst);
    GraphRebuildDelayGuard { previous_ms }
}

#[cfg(any(test, feature = "test-support"))]
async fn apply_graph_rebuild_delay_for_test() {
    let delay_ms = GRAPH_REBUILD_DELAY_MS.load(Ordering::SeqCst);
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
}

async fn try_rebuild_artifact(
    rebuild_coordinator: Arc<RebuildCoordinator>,
    previous_artifact: Arc<GraphIndexArtifact>,
    rebuild_candidate: RebuildCandidate,
) -> RebuildAttempt {
    let RebuildCandidate { worktree, key } = rebuild_candidate;
    let mut task = tokio::spawn(async move {
        rebuild_coordinator
            .get_or_build(key, move || {
                let previous_artifact = Arc::clone(&previous_artifact);
                let worktree = worktree.clone();
                async move {
                    #[cfg(any(test, feature = "test-support"))]
                    apply_graph_rebuild_delay_for_test().await;
                    tokio::task::spawn_blocking(move || {
                        let (artifact, _mode) =
                            spur_graph::store::build::artifact_from_facts_incremental(
                                &previous_artifact,
                                &worktree,
                            )?;
                        Ok(Arc::new(artifact))
                    })
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("in-memory graph rebuild task failed: {error}")
                    })?
                }
            })
            .await
    });

    match tokio::time::timeout(graph_rebuild_latency_budget(), &mut task).await {
        Ok(Ok(Ok(artifact))) => RebuildAttempt::Fresh(artifact),
        Ok(Ok(Err(error))) => {
            tracing::warn!(
                target: "spur_mcp::path_a",
                error = %error,
                "in-memory code graph rebuild failed; serving stale response"
            );
            RebuildAttempt::StaleRebuildFailed
        }
        Ok(Err(error)) => {
            tracing::warn!(
                target: "spur_mcp::path_a",
                error = %error,
                "in-memory code graph rebuild task failed; serving stale response"
            );
            RebuildAttempt::StaleRebuildFailed
        }
        Err(_) => {
            tokio::spawn(async move {
                match task.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(
                            target: "spur_mcp::path_a",
                            error = %error,
                            "in-memory code graph rebuild failed after response budget elapsed"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "spur_mcp::path_a",
                            error = %error,
                            "in-memory code graph rebuild task failed after response budget elapsed"
                        );
                    }
                }
            });
            RebuildAttempt::StaleBudgetExceeded
        }
    }
}

async fn code_graph_response(id: Value, result: CodeGraphResult) -> JsonRpcResponse {
    match result {
        Ok(body) => json_success(id, body),
        Err(error) => code_graph_error_response(id, error).await,
    }
}

async fn code_graph_error_response(id: Value, error: CodeGraphError) -> JsonRpcResponse {
    let CodeGraphError {
        error,
        metadata,
        temporal_code,
        temporal_data,
    } = error;
    // Temporal resolution failures take priority: emit error_with_data with structured payload.
    if let (Some(code), Some(data)) = (temporal_code, temporal_data) {
        let message = match &error {
            McpHandlerError::Internal(msg) => msg.clone(),
            McpHandlerError::NotFound(msg) => msg.clone(),
            McpHandlerError::InvalidParams(msg) => msg.clone(),
            McpHandlerError::Unauthorized(msg) => msg.clone(),
            McpHandlerError::UpstreamPm(msg) => msg.clone(),
        };
        return JsonRpcResponse::error_with_data(id, code, message, *data);
    }
    let mut response = match error {
        McpHandlerError::InvalidParams(message) => JsonRpcResponse::invalid_params(id, message),
        McpHandlerError::NotFound(message) => JsonRpcResponse::error_with_data(
            id,
            CODE_GRAPH_NOT_FOUND_ERROR_CODE,
            message,
            json!({ "kind": "not_found" }),
        ),
        McpHandlerError::Unauthorized(message) => JsonRpcResponse::error(id, -32001, message),
        McpHandlerError::UpstreamPm(message) | McpHandlerError::Internal(message) => {
            JsonRpcResponse::internal_error(id, message)
        }
    };
    if let (Some(error), Some(metadata)) = (response.error.as_mut(), metadata) {
        let metadata = GraphResponseMetadata::from_source(*metadata)
            .await
            .into_value();
        match (&mut error.data, metadata) {
            (Some(Value::Object(data)), Value::Object(metadata)) => {
                data.extend(metadata);
            }
            (_, metadata) => {
                error.data = Some(metadata);
            }
        }
    }
    response
}

#[allow(clippy::result_large_err)]
fn current_worktree() -> Result<PathBuf, McpHandlerError> {
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}

#[allow(clippy::result_large_err)]
fn load_graph_artifact_for_request() -> Result<GraphIndexArtifact, McpHandlerError> {
    let worktree = current_worktree()?;
    let resolved = resolve_artifact_location(&worktree, None)
        .map_err(|_| graph_artifact_missing(&worktree))?;
    let artifact_path = resolved.path;

    match load_artifact(&artifact_path) {
        Ok(artifact) => Ok(artifact),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == ErrorKind::NotFound) =>
        {
            Err(graph_artifact_missing(&worktree))
        }
        Err(_) if !artifact_path.exists() => Err(graph_artifact_missing(&worktree)),
        Err(error) => Err(McpHandlerError::Internal(format!(
            "failed to load graph artifact `{}`: {error}",
            artifact_path.display()
        ))),
    }
}

#[allow(clippy::result_large_err)]
fn load_graph_artifact_for_worktree(
    worktree: &Path,
) -> Result<GraphIndexArtifact, McpHandlerError> {
    let resolved =
        resolve_artifact_location(worktree, None).map_err(|_| graph_artifact_missing(worktree))?;
    let artifact_path = resolved.path;

    match load_artifact(&artifact_path) {
        Ok(artifact) => Ok(artifact),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == ErrorKind::NotFound) =>
        {
            Err(graph_artifact_missing(worktree))
        }
        Err(_) if !artifact_path.exists() => Err(graph_artifact_missing(worktree)),
        Err(error) => Err(McpHandlerError::Internal(format!(
            "failed to load graph artifact `{}`: {error}",
            artifact_path.display()
        ))),
    }
}

fn graph_artifact_missing(worktree: &Path) -> McpHandlerError {
    McpHandlerError::Internal(format!(
        "graph artifact not found; run `spur graph build` in {}",
        worktree.display()
    ))
}

fn load_commit_index_for_request(worktree: &Path) -> Result<CommitIndexArtifact, McpHandlerError> {
    let pointer = spur_graph::store::commit_index::load_pointer(worktree).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index pointer in {}: {error}",
            worktree.display()
        ))
    })?;
    let pointer = pointer.ok_or_else(|| commit_index_missing(worktree))?;
    spur_graph::store::commit_index::load_artifact(worktree, &pointer).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index artifact in {}: {error}",
            worktree.display()
        ))
    })
}

fn commit_index_missing(worktree: &Path) -> McpHandlerError {
    McpHandlerError::Internal(format!(
        "commit index not found; run `spur graph build --history` in {}",
        worktree.display()
    ))
}

fn resolve_symbol_for_optional_as_of(
    artifact: &GraphIndexArtifact,
    worktree: &Path,
    symbol_id: &str,
    args: &Value,
) -> Result<String, CodeGraphError> {
    let Some(as_of) = parse_as_of(args)? else {
        return Ok(symbol_id.to_string());
    };
    let commits = load_commit_index_for_request(worktree)?;
    temporal_resolution_symbol_id(
        symbol_id,
        &as_of,
        resolve_symbol_as_of(artifact, &commits, symbol_id, &as_of)?,
    )
}

fn resolve_symbol_for_optional_as_of_current_worktree(
    artifact: &GraphIndexArtifact,
    symbol_id: &str,
    args: &Value,
) -> Result<String, CodeGraphError> {
    if parse_as_of(args)?.is_none() {
        return Ok(symbol_id.to_string());
    }
    let worktree = current_worktree()?;
    resolve_symbol_for_optional_as_of(artifact, &worktree, symbol_id, args)
}

fn parse_as_of(args: &Value) -> Result<Option<String>, McpHandlerError> {
    match args.get("as_of") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(McpHandlerError::InvalidParams(
            "field 'as_of' must not be empty".to_string(),
        )),
        Some(_) => Err(McpHandlerError::InvalidParams(
            "field 'as_of' must be a string".to_string(),
        )),
    }
}

fn reachable_commits(
    commits: &CommitIndexArtifact,
    as_of: &str,
) -> Result<HashSet<String>, McpHandlerError> {
    if !commits.commits.iter().any(|commit| commit.sha == as_of) {
        return Err(McpHandlerError::InvalidParams(format!(
            "as_of commit `{as_of}` is not indexed"
        )));
    }

    let mut reachable = HashSet::new();
    let mut stack = vec![as_of.to_string()];
    while let Some(sha) = stack.pop() {
        if !reachable.insert(sha.clone()) {
            continue;
        }
        let commit = commits
            .commits
            .iter()
            .find(|commit| commit.sha == sha)
            .ok_or_else(|| {
                McpHandlerError::Internal(format!(
                    "commit index references missing parent commit `{sha}`"
                ))
            })?;
        stack.extend(commit.parents.iter().cloned());
    }

    Ok(reachable)
}

fn temporal_resolution_symbol_id(
    symbol_id: &str,
    as_of: &str,
    resolution: Resolution<String>,
) -> Result<String, CodeGraphError> {
    match resolution {
        Resolution::Found { value, .. } => Ok(value),
        Resolution::Deleted { last_seen } => {
            Err(deleted_resolution_error(symbol_id, as_of, last_seen))
        }
        Resolution::Ambiguous { candidates } => {
            Err(ambiguous_resolution_error(symbol_id, as_of, candidates))
        }
        Resolution::Unknown { reason } => Err(unknown_resolution_error(symbol_id, as_of, reason)),
    }
}

fn resolve_symbol_as_of(
    artifact: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol_id: &str,
    as_of: &str,
) -> Result<Resolution<String>, CodeGraphError> {
    let index = TemporalIndex::new(artifact);
    if !commits.commits.iter().any(|commit| commit.sha == as_of) {
        return Err(McpHandlerError::InvalidParams(format!(
            "as_of commit `{as_of}` is not indexed"
        ))
        .into());
    }

    let history = symbol_history(&index, commits, symbol_id);
    if history.is_empty() {
        return Err(McpHandlerError::NotFound(format!(
            "symbol {symbol_id} has no temporal history in graph artifact"
        ))
        .into());
    }

    let mut last_unknown = None;
    for (_, _, key) in history {
        match resolve_symbol_at(artifact, commits, &key.stable_symbol_id, &key.commit, as_of) {
            Resolution::Found { value, chain } => return Ok(Resolution::Found { value, chain }),
            Resolution::Deleted { last_seen } => return Ok(Resolution::Deleted { last_seen }),
            Resolution::Ambiguous { candidates } => {
                return Ok(Resolution::Ambiguous { candidates });
            }
            Resolution::Unknown { reason } => {
                last_unknown = Some(reason);
            }
        }
    }

    if let Some(reason) = last_unknown {
        return Ok(Resolution::Unknown { reason });
    }

    Err(McpHandlerError::NotFound(format!(
        "symbol {symbol_id} not present at commit `{as_of}`"
    ))
    .into())
}

fn format_resolution_failure(reason: &ResolutionFailure) -> String {
    match reason {
        ResolutionFailure::AnchorCommitNotIndexed(commit) => {
            format!("anchor commit `{commit}` is not indexed")
        }
        ResolutionFailure::SymbolNotPresentAtAnchor => {
            "symbol is not present at anchor commit".to_string()
        }
        ResolutionFailure::IndexCorrupt(message) => format!("index corrupt: {message}"),
    }
}

enum CodeSelectorResolution {
    Resolved(String),
    Ambiguous(Vec<CandidateRow>),
}

enum CodeReadSymbolTarget<'a> {
    Resolved(&'a GraphSymbolArtifact),
    Ambiguous(Vec<CandidateRow>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OnAmbiguousMode {
    Candidates,
    Error,
}

fn resolve_code_selector(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<CodeSelectorResolution, McpHandlerError> {
    let selector = selected_code_selector(args)?;
    let on_ambiguous = on_ambiguous_mode(args)?;

    match resolve_selector(artifact, selector) {
        SelectorResolution::Resolved(resolved) => {
            Ok(CodeSelectorResolution::Resolved(resolved.stable_symbol_id))
        }
        SelectorResolution::Ambiguous { candidates: _ }
            if on_ambiguous == OnAmbiguousMode::Error =>
        {
            Err(McpHandlerError::InvalidParams(format!(
                "selector `{selector}` is ambiguous; choose one candidate selector or uri"
            )))
        }
        SelectorResolution::Ambiguous { candidates } => {
            Ok(CodeSelectorResolution::Ambiguous(candidates))
        }
        SelectorResolution::NotFound => Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(selector)
        ))),
    }
}

fn selected_code_selector(args: &Value) -> Result<&str, McpHandlerError> {
    let selector = string_arg(args, "selector")?;
    let symbol = string_arg(args, "symbol")?;

    match (selector, symbol) {
        (Some(selector), Some(_)) => {
            tracing::warn!(
                "code graph request included deprecated `symbol` with `selector`; using `selector`"
            );
            Ok(selector)
        }
        (Some(selector), None) => Ok(selector),
        (None, Some(symbol)) => {
            tracing::warn!("code graph request used deprecated `symbol`; use `selector`");
            Ok(symbol)
        }
        (None, None) => Err(McpHandlerError::InvalidParams(
            "Missing required field 'selector' (or deprecated 'symbol')".into(),
        )),
    }
}

fn selector_arg(args: &Value) -> Result<&str, McpHandlerError> {
    string_arg(args, "selector")?
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'selector'".into()))
}

fn file_arg(args: &Value) -> Result<&str, McpHandlerError> {
    string_arg(args, "file")?
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'file'".into()))
}

fn code_read_symbol_target<'a>(
    args: &Value,
    artifact: &'a GraphIndexArtifact,
) -> Result<CodeReadSymbolTarget<'a>, McpHandlerError> {
    let stable_symbol_id = string_arg(args, "stable_symbol_id")?;
    let path = string_arg(args, "path")?;
    let name = string_arg(args, "name")?;

    match (stable_symbol_id, path, name) {
        (Some(stable_symbol_id), None, None) => {
            let symbol_id = missing_symbol_label(stable_symbol_id);
            symbol_by_id(artifact, symbol_id).map(CodeReadSymbolTarget::Resolved)
        }
        (None, Some(path), Some(name)) => {
            let path = validate_worktree_relative_path_arg("path", path)?;
            resolve_symbol_by_path_name(artifact, &path, name)
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(McpHandlerError::InvalidParams(
            "field 'stable_symbol_id' is mutually exclusive with fields 'path' and 'name'".into(),
        )),
        (None, Some(_), None) | (None, None, Some(_)) => Err(McpHandlerError::InvalidParams(
            "fields 'path' and 'name' must be provided together".into(),
        )),
        (None, None, None) => Err(McpHandlerError::InvalidParams(
            "Missing required field 'stable_symbol_id' or fields 'path' and 'name'".into(),
        )),
    }
}

fn resolve_symbol_by_path_name<'a>(
    artifact: &'a GraphIndexArtifact,
    path: &str,
    name: &str,
) -> Result<CodeReadSymbolTarget<'a>, McpHandlerError> {
    let matches = artifact
        .symbols
        .iter()
        .filter(|symbol| {
            symbol.file_path == path
                && (symbol.entity_name == name || symbol.qualified_name == name)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(McpHandlerError::NotFound(format!(
            "symbol `{name}` in file `{path}` not found in graph artifact"
        ))),
        [symbol] => Ok(CodeReadSymbolTarget::Resolved(symbol)),
        _ => Ok(CodeReadSymbolTarget::Ambiguous(candidate_rows_for_symbols(
            matches,
        ))),
    }
}

#[derive(Debug)]
struct CodeSearchRequest {
    options: SearchOptions,
    requested_limit: Option<Value>,
}

#[derive(Debug)]
struct CodeTraversalRequest {
    include_unresolved: bool,
}

#[derive(Debug)]
enum CodeSubgraphRoots {
    RootIds(Vec<String>),
    Ambiguous(Vec<CandidateRow>),
}

#[derive(Debug)]
struct CodeSubgraphBudgetRequest {
    budget: SubgraphBudget,
    requested_max_nodes: Option<Value>,
    requested_max_edges: Option<Value>,
}

#[derive(Debug)]
struct ClampedUsizeArg {
    value: usize,
    requested_value: Option<Value>,
}

#[derive(Debug)]
struct LimitArg {
    limit: usize,
    requested_limit: Option<Value>,
}

fn code_search_options(args: &Value) -> Result<CodeSearchRequest, McpHandlerError> {
    let query = query_arg(args)?;
    let mode = search_mode_arg(args)?;
    let symbol_kind = string_arg(args, "symbol_kind")?.map(str::to_string);
    let file = string_arg(args, "file")?
        .map(validate_file_path_arg)
        .transpose()?;
    let file_glob = string_arg(args, "file_glob")?
        .map(validate_file_glob_arg)
        .transpose()?;
    if file.is_some() && file_glob.is_some() {
        return Err(McpHandlerError::InvalidParams(
            "fields 'file' and 'file_glob' are mutually exclusive".into(),
        ));
    }
    let limit = limit_arg(args)?;

    Ok(CodeSearchRequest {
        options: SearchOptions {
            query,
            mode,
            filters: SearchFilters {
                symbol_kind,
                file,
                file_glob,
            },
            limit: limit.limit,
        },
        requested_limit: limit.requested_limit,
    })
}

fn code_traversal_request(args: &Value) -> Result<CodeTraversalRequest, McpHandlerError> {
    Ok(CodeTraversalRequest {
        include_unresolved: bool_arg(args, "include_unresolved")?.unwrap_or(false),
    })
}

fn code_subgraph_root_ids(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<CodeSubgraphRoots, CodeGraphError> {
    if let Some(start_nodes) = start_nodes_arg(args)? {
        if string_arg(args, "selector")?.is_some() || string_arg(args, "symbol")?.is_some() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' is mutually exclusive with 'selector' and 'symbol'".into(),
            )
            .into());
        }
        for node_id in &start_nodes {
            ensure_symbol_id_exists(artifact, node_id)?;
        }
        return Ok(CodeSubgraphRoots::RootIds(start_nodes));
    }

    match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => Ok(CodeSubgraphRoots::RootIds(vec![
            resolve_symbol_for_optional_as_of_current_worktree(artifact, &symbol_id, args)?,
        ])),
        CodeSelectorResolution::Ambiguous(candidates) => {
            Ok(CodeSubgraphRoots::Ambiguous(candidates))
        }
    }
}

fn start_nodes_arg(args: &Value) -> Result<Option<Vec<String>>, McpHandlerError> {
    let Some(value) = args.get("start_nodes") else {
        return Ok(None);
    };
    let nodes = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams("field 'start_nodes' must be an array of strings".into())
    })?;
    if nodes.is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "field 'start_nodes' must contain at least one node id".into(),
        ));
    }

    let mut seen = HashSet::new();
    let mut start_nodes = Vec::new();
    for node in nodes {
        let node = node.as_str().ok_or_else(|| {
            McpHandlerError::InvalidParams("field 'start_nodes' must be an array of strings".into())
        })?;
        if node.trim().is_empty() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' must not contain empty node ids".into(),
            ));
        }
        let node_id = missing_symbol_label(node);
        if node_id.trim().is_empty() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' must not contain empty node ids".into(),
            ));
        }
        let node_id = node_id.to_string();
        if seen.insert(node_id.clone()) {
            start_nodes.push(node_id);
        }
    }

    Ok(Some(start_nodes))
}

fn ensure_symbol_id_exists(
    artifact: &GraphIndexArtifact,
    symbol_id: &str,
) -> Result<(), McpHandlerError> {
    if artifact
        .symbols
        .iter()
        .any(|symbol| symbol.stable_symbol_id == symbol_id)
    {
        Ok(())
    } else {
        Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(symbol_id)
        )))
    }
}

fn code_subgraph_budget(args: &Value) -> Result<CodeSubgraphBudgetRequest, McpHandlerError> {
    let max_nodes = clamped_usize_arg(
        args,
        "max_nodes",
        DEFAULT_MCP_CODE_SUBGRAPH_MAX_NODES,
        MIN_MCP_CODE_SUBGRAPH_MAX_NODES,
        MAX_MCP_CODE_SUBGRAPH_MAX_NODES,
    )?;
    let max_edges = clamped_usize_arg(
        args,
        "max_edges",
        DEFAULT_MCP_CODE_SUBGRAPH_MAX_EDGES,
        MIN_MCP_CODE_SUBGRAPH_MAX_EDGES,
        MAX_MCP_CODE_SUBGRAPH_MAX_EDGES,
    )?;

    Ok(CodeSubgraphBudgetRequest {
        budget: SubgraphBudget {
            max_nodes: max_nodes.value,
            max_edges: max_edges.value,
        },
        requested_max_nodes: max_nodes.requested_value,
        requested_max_edges: max_edges.requested_value,
    })
}

fn clamped_usize_arg(
    args: &Value,
    field: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<ClampedUsizeArg, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(ClampedUsizeArg {
            value: default,
            requested_value: None,
        });
    };
    if let Some(limit) = value.as_i64() {
        let clamped = limit.clamp(min as i64, max as i64);
        return Ok(ClampedUsizeArg {
            value: clamped as usize,
            requested_value: (limit != clamped).then(|| json!(limit)),
        });
    }
    if let Some(limit) = value.as_u64() {
        let clamped = limit.clamp(min as u64, max as u64);
        return Ok(ClampedUsizeArg {
            value: clamped as usize,
            requested_value: (limit != clamped).then(|| json!(limit)),
        });
    }

    Err(McpHandlerError::InvalidParams(format!(
        "field '{field}' must be an integer"
    )))
}

fn code_subgraph_metadata(
    radius: u8,
    truncated: bool,
    budget: &CodeSubgraphBudgetRequest,
) -> Value {
    let mut metadata = json!({
        "radius": radius,
        "max_nodes": budget.budget.max_nodes,
        "max_edges": budget.budget.max_edges,
        "truncated": truncated,
    });
    if let Some(requested_max_nodes) = &budget.requested_max_nodes {
        metadata["requested_max_nodes"] = requested_max_nodes.clone();
    }
    if let Some(requested_max_edges) = &budget.requested_max_edges {
        metadata["requested_max_edges"] = requested_max_edges.clone();
    }
    metadata
}

fn symbol_id_arg(args: &Value) -> Result<String, McpHandlerError> {
    let value = args
        .get("symbol")
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'symbol'".into()))?;
    let s = value
        .as_str()
        .ok_or_else(|| McpHandlerError::InvalidParams("field 'symbol' must be a string".into()))?
        .trim();
    if s.is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "field 'symbol' must not be empty".into(),
        ));
    }
    // Strip the URI prefix if present so callers work with bare IDs.
    Ok(missing_symbol_label(s).to_string())
}

fn query_arg(args: &Value) -> Result<String, McpHandlerError> {
    let value = args
        .get("query")
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'query'".into()))?;
    let query = value
        .as_str()
        .ok_or_else(|| McpHandlerError::InvalidParams("field 'query' must be a string".into()))?
        .trim();
    if query.is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "field 'query' must not be empty".into(),
        ));
    }
    Ok(query.to_string())
}

fn search_mode_arg(args: &Value) -> Result<SearchMode, McpHandlerError> {
    let Some(value) = args.get("mode") else {
        return Ok(SearchMode::Substring);
    };
    match value.as_str() {
        Some("exact") => Ok(SearchMode::Exact),
        Some("prefix") => Ok(SearchMode::Prefix),
        Some("substring") => Ok(SearchMode::Substring),
        Some(other) => Err(McpHandlerError::InvalidParams(format!(
            "invalid mode `{other}`; expected `exact`, `prefix`, or `substring`"
        ))),
        None => Err(McpHandlerError::InvalidParams(
            "field 'mode' must be a string".into(),
        )),
    }
}

fn limit_arg(args: &Value) -> Result<LimitArg, McpHandlerError> {
    let Some(value) = args.get("limit") else {
        return Ok(LimitArg {
            limit: 20,
            requested_limit: None,
        });
    };
    if let Some(limit) = value.as_i64() {
        let clamped = limit.clamp(1, 200);
        return Ok(LimitArg {
            limit: clamped as usize,
            requested_limit: (limit != clamped).then(|| json!(limit)),
        });
    }
    if let Some(limit) = value.as_u64() {
        let clamped = limit.clamp(1, 200);
        return Ok(LimitArg {
            limit: clamped as usize,
            requested_limit: (limit != clamped).then(|| json!(limit)),
        });
    }
    Err(McpHandlerError::InvalidParams(
        "field 'limit' must be an integer".into(),
    ))
}

fn search_mode_str(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Exact => "exact",
        SearchMode::Prefix => "prefix",
        SearchMode::Substring => "substring",
    }
}

fn validate_file_path_arg(file: &str) -> Result<String, McpHandlerError> {
    validate_worktree_relative_path_arg("file", file)
}

fn validate_file_glob_arg(file_glob: &str) -> Result<String, McpHandlerError> {
    validate_worktree_relative_path_arg("file_glob", file_glob)
}

fn validate_worktree_relative_path_arg(
    field: &str,
    value: &str,
) -> Result<String, McpHandlerError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a worktree-relative path"
        )));
    }

    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(McpHandlerError::InvalidParams(format!(
                        "field '{field}' must be a UTF-8 path"
                    )));
                };
                normalized.push(part);
            }
            Component::CurDir => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must not contain '.' path components"
                )));
            }
            Component::ParentDir => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must not contain '..' path components"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must be a worktree-relative path"
                )));
            }
        }
    }

    let normalized = normalized.join("/");
    if normalized != value {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a normalized worktree-relative path without '.' or '..' components"
        )));
    }

    Ok(normalized)
}

fn string_arg<'a>(args: &'a Value, field: &str) -> Result<Option<&'a str>, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!("field '{field}' must be a string"))
    })?;
    if value.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must not be empty"
        )));
    }
    Ok(Some(value))
}

fn bool_arg(args: &Value, field: &str) -> Result<Option<bool>, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| McpHandlerError::InvalidParams(format!("field '{field}' must be a boolean")))
}

fn on_ambiguous_mode(args: &Value) -> Result<OnAmbiguousMode, McpHandlerError> {
    let Some(value) = args.get("on_ambiguous") else {
        return Ok(OnAmbiguousMode::Candidates);
    };
    match value.as_str() {
        Some("candidates") => Ok(OnAmbiguousMode::Candidates),
        Some("error") => Ok(OnAmbiguousMode::Error),
        Some(other) => Err(McpHandlerError::InvalidParams(format!(
            "invalid on_ambiguous `{other}`; expected `candidates` or `error`"
        ))),
        None => Err(McpHandlerError::InvalidParams(
            "field 'on_ambiguous' must be a string".into(),
        )),
    }
}

fn missing_symbol_label(selector: &str) -> &str {
    selector
        .strip_prefix(CODE_SYMBOL_URI_PREFIX)
        .unwrap_or(selector)
}

fn parse_edge_kinds(args: &Value) -> Result<Option<Vec<GraphEdgeKind>>, McpHandlerError> {
    let Some(value) = args.get("edge_kinds") else {
        return Ok(None);
    };
    let kinds = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams("field 'edge_kinds' must be an array of strings".to_string())
    })?;
    kinds
        .iter()
        .map(|kind| {
            let kind = kind.as_str().ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "field 'edge_kinds' must be an array of strings".to_string(),
                )
            })?;
            serde_json::from_value::<GraphEdgeKind>(Value::String(kind.to_string())).map_err(
                |_| {
                    McpHandlerError::InvalidParams(format!(
                        "invalid edge kind `{kind}`; expected one of calls, calls_dyn, references_hof, references_other"
                    ))
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn resolve_candidate_rows(
    artifact: &GraphIndexArtifact,
    selector: &str,
) -> Result<Vec<CandidateRow>, McpHandlerError> {
    match resolve_selector(artifact, selector) {
        SelectorResolution::Resolved(resolved) => {
            let symbol = symbol_by_id(artifact, &resolved.stable_symbol_id)?;
            Ok(vec![candidate_row_for_symbol(symbol)])
        }
        SelectorResolution::Ambiguous { candidates } => Ok(candidates),
        SelectorResolution::NotFound => Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(selector)
        ))),
    }
}

fn symbol_by_id<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Result<&'a GraphSymbolArtifact, McpHandlerError> {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.stable_symbol_id == symbol_id)
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "resolved symbol id `{symbol_id}` missing from graph artifact"
            ))
        })
}

fn file_manifest_for_symbol<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol: &GraphSymbolArtifact,
) -> Result<&'a GraphFileManifestEntry, McpHandlerError> {
    artifact
        .file_manifests
        .iter()
        .find(|entry| entry.path == symbol.file_path)
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "graph artifact has no file manifest for `{}`",
                symbol.file_path
            ))
        })
}

fn read_indexed_file_bytes(
    worktree: &Path,
    file_path: &str,
    content_oid: &str,
) -> Result<Vec<u8>, McpHandlerError> {
    if content_oid.starts_with("gitlink:") {
        return Err(McpHandlerError::Internal(format!(
            "indexed source for `{file_path}` points to gitlink `{content_oid}`"
        )));
    }
    if let Some(bytes) = read_git_blob(worktree, content_oid)? {
        return Ok(bytes);
    }

    let current = read_current_file_bytes(worktree, file_path)?;
    if git_blob_oid(&current) == content_oid {
        return Ok(current);
    }

    Err(McpHandlerError::Internal(format!(
        "indexed blob `{content_oid}` for `{file_path}` is not available in git object storage"
    )))
}

fn read_git_blob(worktree: &Path, content_oid: &str) -> Result<Option<Vec<u8>>, McpHandlerError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["cat-file", "-p", content_oid])
        .output()
        .map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to read indexed blob `{content_oid}`: {error}"
            ))
        })?;

    if output.status.success() {
        Ok(Some(output.stdout))
    } else {
        Ok(None)
    }
}

fn current_file_oid(worktree: &Path, file_path: &str) -> Result<Option<String>, McpHandlerError> {
    match fs::read(worktree.join(file_path)) {
        Ok(bytes) => Ok(Some(git_blob_oid(&bytes))),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => {
            Ok(None)
        }
        Err(error) => Err(McpHandlerError::Internal(format!(
            "failed to read current file `{}`: {error}",
            worktree.join(file_path).display()
        ))),
    }
}

fn read_current_file_bytes(worktree: &Path, file_path: &str) -> Result<Vec<u8>, McpHandlerError> {
    fs::read(worktree.join(file_path)).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to read current file `{}` while resolving indexed blob: {error}",
            worktree.join(file_path).display()
        ))
    })
}

fn source_range_with_context(
    source: &str,
    symbol: &GraphSymbolArtifact,
    context_lines: usize,
) -> [usize; 2] {
    let line_count = source.split_inclusive('\n').count();
    let symbol_start = symbol.line_range[0].max(1);
    let symbol_end = symbol.line_range[1].max(symbol_start);
    let start = symbol_start.saturating_sub(context_lines).max(1);
    let end = symbol_end
        .saturating_add(context_lines)
        .min(line_count)
        .max(start.saturating_sub(1));
    [start, end]
}

fn source_for_line_range(source: &str, line_range: [usize; 2]) -> String {
    let [start, end] = line_range;
    source
        .split_inclusive('\n')
        .enumerate()
        .filter_map(|(index, line)| {
            let line_no = index + 1;
            (start <= line_no && line_no <= end).then_some(line)
        })
        .collect()
}

fn candidate_rows_for_symbols<'a>(
    symbols: impl IntoIterator<Item = &'a GraphSymbolArtifact>,
) -> Vec<CandidateRow> {
    let mut rows = symbols
        .into_iter()
        .map(candidate_row_for_symbol)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
            .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

fn candidate_row_for_symbol(symbol: &GraphSymbolArtifact) -> CandidateRow {
    let uri = format!("{CODE_SYMBOL_URI_PREFIX}{}", symbol.stable_symbol_id);
    let selector = if symbol.qualified_name.is_empty() {
        uri.clone()
    } else {
        format!("{}::{}", symbol.file_path, symbol.qualified_name)
    };

    CandidateRow {
        selector,
        uri,
        id: symbol.stable_symbol_id.clone(),
        entity_name: symbol.entity_name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
    }
}

fn ambiguous_response(candidates: Vec<CandidateRow>) -> Value {
    json!({
        "ambiguous": true,
        "candidates": candidates.into_iter().map(candidate_row).collect::<Vec<_>>(),
    })
}

async fn with_graph_metadata(artifact: &GraphIndexArtifact, mut body: Value) -> Value {
    let files = response_file_set_from_body(artifact, &body);
    GraphResponseMetadata::from_artifact_with_files(artifact, &files)
        .await
        .insert_into(&mut body);
    body
}

async fn with_graph_metadata_with_files(
    artifact: &GraphIndexArtifact,
    mut body: Value,
    files: &[(String, String)],
) -> Value {
    GraphResponseMetadata::from_artifact_with_files(artifact, files)
        .await
        .insert_into(&mut body);
    body
}

fn response_file_set_from_body(
    artifact: &GraphIndexArtifact,
    body: &Value,
) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    collect_response_file_paths(body, &mut paths);
    response_file_set_for_paths(artifact, paths)
}

fn empty_code_search_file_set(
    artifact: &GraphIndexArtifact,
    body: &Value,
) -> Option<Vec<(String, String)>> {
    let candidates = body.get("candidates")?.as_array()?;
    if !candidates.is_empty() || body.get("query").is_none() || body.get("total_matches").is_none()
    {
        return None;
    }

    if let Some(file) = body.get("file").and_then(Value::as_str) {
        return Some(response_file_set_for_paths(artifact, [file]));
    }

    Some(all_indexed_file_set(artifact))
}

fn all_indexed_file_set(artifact: &GraphIndexArtifact) -> Vec<(String, String)> {
    artifact
        .file_manifests
        .iter()
        .map(|entry| (entry.path.clone(), entry.content_oid.clone()))
        .collect()
}

fn response_file_set_for_symbol_ids<'a>(
    artifact: &GraphIndexArtifact,
    symbol_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let paths = symbol_ids
        .into_iter()
        .filter_map(|symbol_id| symbol_by_id(artifact, symbol_id).ok())
        .map(|symbol| symbol.file_path.as_str());
    response_file_set_for_paths(artifact, paths)
}

fn response_file_set_for_paths<'a>(
    artifact: &GraphIndexArtifact,
    paths: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for path in paths {
        if !seen.insert(path.to_string()) {
            continue;
        }
        let Some(indexed_oid) = indexed_file_oid_for_path(artifact, path) else {
            continue;
        };
        files.push((path.to_string(), indexed_oid.to_string()));
    }
    files
}

fn collect_response_file_paths<'a>(value: &'a Value, paths: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            if let Some(file_path) = map.get("file_path").and_then(Value::as_str) {
                paths.push(file_path);
            }
            for value in map.values() {
                collect_response_file_paths(value, paths);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_response_file_paths(value, paths);
            }
        }
        _ => {}
    }
}

fn indexed_file_oid_for_path<'a>(artifact: &'a GraphIndexArtifact, path: &str) -> Option<&'a str> {
    artifact
        .file_manifests
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.content_oid.as_str())
}

fn current_worktree_root() -> Option<std::path::PathBuf> {
    std::env::current_dir().ok().map(resolve_worktree_root_from)
}

fn matching_graph_pointer(
    worktree: &Path,
    source: &GraphMetadataSource,
) -> Option<GraphIndexPointer> {
    let pointer_path = worktree.join(GRAPH_POINTER_RELATIVE_PATH);
    let bytes = std::fs::read(pointer_path).ok()?;
    let pointer: GraphIndexPointer = serde_json::from_slice(&bytes).ok()?;
    if pointer.graph_content_hash == source.graph_content_hash
        && pointer.manifest_version == source.manifest_version
    {
        Some(pointer)
    } else {
        None
    }
}

fn graph_built_at_from_pointer(pointer: &GraphIndexPointer) -> Option<String> {
    let modified = std::fs::metadata(&pointer.canonical_artifact_path)
        .ok()?
        .modified()
        .ok()?;
    let built_at = DateTime::<Utc>::from(modified);
    Some(built_at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn non_empty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

#[derive(Debug)]
struct WorktreeGitMetadata {
    head_oid: String,
    has_uncommitted_changes: bool,
}

async fn worktree_git_metadata(worktree: &Path) -> Option<WorktreeGitMetadata> {
    tokio::time::timeout(GRAPH_GIT_METADATA_TIMEOUT, async {
        let head_oid = run_git_stdout(worktree, &["rev-parse", "HEAD"]).await?;
        // `--untracked-files=no` scopes dirtiness to tracked changes: the flag
        // answers "is the graph trustworthy?", not "does the filesystem have
        // new files?". Untracked artifacts (logs, scratch RCAs) routinely
        // litter active worktrees and would otherwise pin the flag to `true`.
        let status =
            run_git_stdout(worktree, &["status", "--porcelain", "--untracked-files=no"]).await?;
        Some(WorktreeGitMetadata {
            head_oid,
            has_uncommitted_changes: !status.is_empty(),
        })
    })
    .await
    .ok()
    .flatten()
}

async fn run_git_stdout(worktree: &Path, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(worktree).kill_on_drop(true);
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

fn compute_worktree_dirty(
    indexed_head_oid: Option<&str>,
    worktree_head_oid: &str,
    has_uncommitted_changes: bool,
) -> Option<bool> {
    if has_uncommitted_changes {
        Some(true)
    } else {
        indexed_head_oid.map(|indexed_head_oid| indexed_head_oid != worktree_head_oid)
    }
}

fn candidate_row(candidate: CandidateRow) -> Value {
    json!({
        "selector": candidate.selector,
        "uri": candidate.uri,
        "id": candidate.id,
        "entity_name": candidate.entity_name,
        "qualified_name": candidate.qualified_name,
        "file_path": candidate.file_path,
        "line_range": candidate.line_range,
        "symbol_kind": candidate.symbol_kind,
        "enclosing_scope": candidate.enclosing_scope,
    })
}

fn symbol_info_row(symbol: &GraphSymbolArtifact) -> Value {
    json!({
        "qualified_name": symbol.qualified_name,
        "entity_name": symbol.entity_name,
        "file_path": symbol.file_path,
        "line_range": symbol.line_range,
        "symbol_kind": symbol.symbol_kind,
        "enclosing_scope": symbol.enclosing_scope,
        "uri": symbol_uri(&symbol.stable_symbol_id),
        "id": symbol.stable_symbol_id,
    })
}

fn symbol_row(symbol: &GraphSymbolArtifact) -> Value {
    json!({
        "uri": symbol_uri(&symbol.stable_symbol_id),
        "entity_name": symbol.entity_name,
        "enclosing_scope": symbol.enclosing_scope,
        "file_path": symbol.file_path,
        "line_range": symbol.line_range,
        "symbol_kind": symbol.symbol_kind,
    })
}

#[derive(Debug)]
struct TraversalSummary {
    counts_by_kind: Value,
    unresolved_sample: Vec<String>,
}

fn caller_summary(records: &[CallerRecord<'_>]) -> TraversalSummary {
    let unresolved = records.iter().filter_map(|record| match record {
        CallerRecord::Unresolved { target_label, .. } => Some(target_label.as_str()),
        CallerRecord::Resolved { .. } => None,
    });
    traversal_summary(records.iter().map(CallerRecord::edge), unresolved)
}

fn callee_summary(records: &[CalleeRecord<'_>]) -> TraversalSummary {
    let unresolved = records.iter().filter_map(|record| match record {
        CalleeRecord::Unresolved { target_label, .. } => Some(target_label.as_str()),
        CalleeRecord::Resolved { .. } => None,
    });
    traversal_summary(records.iter().map(CalleeRecord::edge), unresolved)
}

fn traversal_summary<'a>(
    edges: impl IntoIterator<Item = &'a GraphEdgeArtifact>,
    unresolved_labels: impl IntoIterator<Item = &'a str>,
) -> TraversalSummary {
    let mut calls = 0usize;
    let mut calls_dyn = 0usize;
    let mut references_hof = 0usize;
    let mut references_other = 0usize;
    let mut unresolved = 0usize;

    for edge in edges {
        match edge_kind(edge) {
            GraphEdgeKind::Calls => calls += 1,
            GraphEdgeKind::CallsDyn => calls_dyn += 1,
            GraphEdgeKind::ReferencesHof => references_hof += 1,
            GraphEdgeKind::ReferencesOther => references_other += 1,
        }
        if edge.target_stable_symbol_id.is_none() {
            unresolved += 1;
        }
    }

    TraversalSummary {
        counts_by_kind: json!({
            "calls": calls,
            "calls_dyn": calls_dyn,
            "references_hof": references_hof,
            "references_other": references_other,
            "unresolved": unresolved,
        }),
        unresolved_sample: unresolved_sample(unresolved_labels),
    }
}

fn unresolved_sample<'a>(labels: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut sample = Vec::new();
    let mut bytes = 0usize;

    for label in labels {
        if sample.len() >= 5 || !seen.insert(label) {
            continue;
        }
        let next_bytes = bytes + label.len();
        if next_bytes > 120 {
            break;
        }
        bytes = next_bytes;
        sample.push(label.to_string());
    }

    sample
}

fn caller_row(caller: CallerRecord<'_>) -> Value {
    match caller {
        CallerRecord::Resolved { caller, edge } => {
            let mut row = symbol_row(caller);
            add_edge_metadata(&mut row, edge, true, None);
            row
        }
        CallerRecord::Unresolved {
            caller,
            edge,
            target_label,
        } => {
            let mut row = symbol_row(caller);
            add_edge_metadata(&mut row, edge, false, Some(target_label));
            row
        }
    }
}

fn callee_row(callee: CalleeRecord<'_>) -> Value {
    match callee {
        CalleeRecord::Resolved { symbol, edge } => {
            let mut row = symbol_row(symbol);
            add_edge_metadata(&mut row, edge, true, None);
            row
        }
        CalleeRecord::Unresolved { edge, target_label } => {
            let entity_name = target_label.clone();
            let mut row = json!({
                "resolved": false,
                "entity_name": entity_name,
                "target_label": target_label,
            });
            add_edge_metadata(&mut row, edge, false, None);
            row
        }
    }
}

fn add_edge_metadata(
    row: &mut Value,
    edge: &GraphEdgeArtifact,
    resolved: bool,
    unresolved_target_label: Option<String>,
) {
    let Some(map) = row.as_object_mut() else {
        return;
    };
    map.insert("resolved".to_string(), Value::Bool(resolved));
    let kind = edge_kind(edge);
    map.insert(
        "edge_kind".to_string(),
        Value::String(edge_kind_str(kind).to_string()),
    );
    if let Some(target_label) = unresolved_target_label {
        map.insert("target_label".to_string(), Value::String(target_label));
    }
    if let Some(bind_method) = &edge.bind_method {
        map.insert(
            "bind_method".to_string(),
            Value::String(bind_method.clone()),
        );
        map.insert("confidence".to_string(), json!(edge.confidence));
    } else if kind == GraphEdgeKind::CallsDyn {
        map.insert("confidence".to_string(), json!(edge.confidence));
    }
}

fn edge_row(edge: &GraphEdgeArtifact) -> Value {
    json!({
        "source_uri": symbol_uri(&edge.source_stable_symbol_id),
        "target_uri": edge.target_stable_symbol_id.as_ref().map(|id| symbol_uri(id)),
        "target_label": edge.target_label,
        "resolved": edge.target_stable_symbol_id.is_some(),
        "relation": edge.relation,
        "edge_kind": edge_kind_str(edge_kind(edge)),
        "confidence": edge.confidence,
        "confidence_score": edge.confidence_score,
        "bind_method": edge.bind_method.clone(),
    })
}

fn edge_kind_str(edge_kind: GraphEdgeKind) -> &'static str {
    match edge_kind {
        GraphEdgeKind::Calls => "calls",
        GraphEdgeKind::CallsDyn => "calls_dyn",
        GraphEdgeKind::ReferencesHof => "references_hof",
        GraphEdgeKind::ReferencesOther => "references_other",
    }
}

fn symbol_uri(symbol_id: &str) -> String {
    format!("{CODE_SYMBOL_URI_PREFIX}{symbol_id}")
}

fn json_success(id: Value, body: Value) -> JsonRpcResponse {
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

fn mermaid_subgraph(nodes: &[&GraphSymbolArtifact], edges: &[&GraphEdgeArtifact]) -> String {
    let mut lines = vec!["graph TD".to_string()];
    for symbol in nodes {
        lines.push(format!(
            "    {}[\"{}\"]",
            mermaid_id(&symbol.stable_symbol_id),
            escape_mermaid_label(&symbol.entity_name)
        ));
    }
    for edge in edges {
        let Some(target_id) = edge.target_stable_symbol_id.as_deref() else {
            continue;
        };
        lines.push(format!(
            "    {} --> {}",
            mermaid_id(&edge.source_stable_symbol_id),
            mermaid_id(target_id)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn mermaid_id(symbol_id: &str) -> String {
    symbol_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn escape_mermaid_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::{json, Value};
    use spur_acp::{BrainSessionId, SessionId};
    use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
    use spur_graph::{
        write_artifact_parquet, write_current_pointer, Confidence, GraphEdgeArtifact,
        GraphEdgeKind, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
        GraphIndexHeader, GraphSymbolArtifact, NodeId, RelationKind, WriteOptions,
    };
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn enter_dir(path: &std::path::Path) -> CwdGuard {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        CwdGuard { original }
    }

    fn no_op_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    fn test_server() -> McpCallbackServer {
        let session_id = BrainSessionId::new(SessionId("brain-test".into()));
        let (server, _channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            community_feature_gate(),
        );
        server
    }

    fn write_fixture_artifact(dir: &TempDir) {
        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": {
                    "graph_index_version": GRAPH_INDEX_VERSION_TEMPORAL
                },
                "manifest_version": "test",
                "graph_content_hash": "test",
                "files": [
                    { "stable_file_id": "file-src-caller", "file_path": "src/caller.rs" },
                    { "stable_file_id": "file-src-root", "file_path": "src/root.rs" },
                    { "stable_file_id": "file-src-callee", "file_path": "src/callee.rs" },
                    { "stable_file_id": "file-src-search", "file_path": "src/search.rs" },
                    { "stable_file_id": "file-crates-foo", "file_path": "crates/foo" },
                    { "stable_file_id": "file-crates-other", "file_path": "crates/other" }
                ],
                "symbols": [
                    symbol("caller", "src/caller.rs", [3, 5], "call_root", "call_root"),
                    symbol("unresolved-caller", "src/caller.rs", [6, 8], "call_root_unresolved", "call_root_unresolved"),
                    symbol("root", "src/root.rs", [10, 12], "root", "root"),
                    symbol("callee", "src/callee.rs", [20, 22], "callee", "callee"),
                    symbol("dyn-callee", "src/callee.rs", [23, 25], "dyn_callee", "dyn_callee"),
                    symbol("hof-callee", "src/callee.rs", [26, 28], "hof_callee", "hof_callee"),
                    symbol("cache-caller", "crates/foo", [24, 26], "call_cache", "call_cache"),
                    symbol("cache-run", "crates/foo", [30, 32], "run", "Cache::run"),
                    symbol("cache-callee", "crates/foo", [34, 36], "finish_cache", "finish_cache"),
                    symbol("mixed-root", "src/root.rs", [50, 52], "mixed_root", "mixed_root"),
                    symbol("mixed-callee", "src/callee.rs", [60, 62], "mixed_callee", "mixed_callee"),
                    symbol("other-run", "crates/other", [40, 42], "run", "Other::run"),
                    symbol("search-submit", "src/search.rs", [70, 72], "submit", "submit"),
                    symbol("search-submit-plan", "src/search.rs", [80, 82], "submit_plan", "submit_plan"),
                    symbol_kind("search-submit-tool", "src/search.rs", [84, 84], "submit_plan", "submit_plan", "mcp_tool")
                ],
                "edges": [
                    edge("caller", "root"),
                    unresolved_edge("unresolved-caller", "root"),
                    edge("root", "callee"),
                    edge_with_kind("root", "dyn-callee", "calls", "calls_dyn"),
                    edge_with_kind("root", "hof-callee", "references", "references_hof"),
                    edge("cache-caller", "cache-run"),
                    edge("cache-run", "cache-callee"),
                    edge("mixed-root", "mixed-callee"),
                    unresolved_edge("mixed-root", "into")
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
    }

    fn write_wide_subgraph_artifact(dir: &TempDir, child_count: usize, edge_count: usize) {
        let child_ids = (0..child_count)
            .map(|index| format!("wide-child-{index:03}"))
            .collect::<Vec<_>>();
        let mut symbols = vec![symbol(
            "wide-root",
            "src/wide.rs",
            [1, 10],
            "wide_root",
            "wide_root",
        )];
        symbols.extend(child_ids.iter().enumerate().map(|(index, id)| {
            symbol(
                id,
                "src/wide.rs",
                [20 + index, 20 + index],
                &format!("wide_child_{index:03}"),
                &format!("wide_child_{index:03}"),
            )
        }));
        let edges = (0..edge_count)
            .map(|index| edge("wide-root", &child_ids[index % child_ids.len()]))
            .collect::<Vec<_>>();

        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": {
                    "graph_index_version": GRAPH_INDEX_VERSION_TEMPORAL
                },
                "manifest_version": "test",
                "graph_content_hash": "wide-test",
                "files": [
                    { "stable_file_id": "file-src-wide", "file_path": "src/wide.rs" }
                ],
                "symbols": symbols,
                "edges": edges,
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
    }

    fn write_current_parquet_fixture(dir: &TempDir) {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "parquet-test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "parquet-test".to_string(),
            graph_content_hash: "parquet-handler-test".to_string(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: "file-src-parquet".to_string(),
                path: "src/parquet.rs".to_string(),
                content_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                node_ids: vec![NodeId(11), NodeId(12)],
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: "file-src-parquet".to_string(),
                file_path: "src/parquet.rs".to_string(),
            }],
            file_node_ids: vec![NodeId(10)],
            symbols: vec![
                GraphSymbolArtifact {
                    stable_symbol_id: "parquet-root".to_string(),
                    file_path: "src/parquet.rs".to_string(),
                    byte_range: [0, 10],
                    line_range: [3, 5],
                    entity_name: "parquet_root".to_string(),
                    qualified_name: "parquet_root".to_string(),
                    symbol_kind: "function".to_string(),
                    anchor_hash: "hash-parquet-root".to_string(),
                    enclosing_scope: None,
                },
                GraphSymbolArtifact {
                    stable_symbol_id: "parquet-child".to_string(),
                    file_path: "src/parquet.rs".to_string(),
                    byte_range: [20, 30],
                    line_range: [8, 9],
                    entity_name: "parquet_child".to_string(),
                    qualified_name: "parquet_child".to_string(),
                    symbol_kind: "function".to_string(),
                    anchor_hash: "hash-parquet-child".to_string(),
                    enclosing_scope: None,
                },
            ],
            symbol_node_ids: vec![NodeId(11), NodeId(12)],
            edges: vec![GraphEdgeArtifact {
                source_stable_symbol_id: "parquet-root".to_string(),
                target_stable_symbol_id: Some("parquet-child".to_string()),
                target_label: Some("parquet_child".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::SyntaxExact,
                confidence_score: 1.0,
                change_kind: None,
                edge_kind: Some(GraphEdgeKind::Calls),
                bind_method: None,
            }],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        };
        let artifact_base = dir.path().join(".git/spur-graph/artifacts/parquet-test");
        let parquet_dir =
            write_artifact_parquet(&artifact, &artifact_base, WriteOptions::default())
                .expect("write parquet artifact");
        write_current_pointer(dir.path(), &parquet_dir).expect("write CURRENT pointer");
    }

    fn symbol(
        id: &str,
        file_path: &str,
        line_range: [usize; 2],
        entity_name: &str,
        qualified_name: &str,
    ) -> Value {
        json!({
            "stable_symbol_id": id,
            "file_path": file_path,
            "byte_range": [0, 8],
            "line_range": line_range,
            "entity_name": entity_name,
            "qualified_name": qualified_name,
            "symbol_kind": "function",
            "anchor_hash": format!("hash-{id}"),
            "enclosing_scope": null
        })
    }

    fn symbol_kind(
        id: &str,
        file_path: &str,
        line_range: [usize; 2],
        entity_name: &str,
        qualified_name: &str,
        symbol_kind: &str,
    ) -> Value {
        let mut symbol = symbol(id, file_path, line_range, entity_name, qualified_name);
        symbol["symbol_kind"] = Value::String(symbol_kind.to_string());
        symbol
    }

    fn edge(source: &str, target: &str) -> Value {
        edge_with_kind(source, target, "calls", "calls")
    }

    fn edge_with_kind(source: &str, target: &str, relation: &str, edge_kind: &str) -> Value {
        json!({
            "source_stable_symbol_id": source,
            "target_stable_symbol_id": target,
            "target_label": null,
            "relation": relation,
            "confidence": "syntax_exact",
            "confidence_score": 1.0,
            "edge_kind": edge_kind
        })
    }

    fn unresolved_edge(source: &str, target_label: &str) -> Value {
        json!({
            "source_stable_symbol_id": source,
            "target_stable_symbol_id": null,
            "target_label": target_label,
            "relation": "calls",
            "confidence": "syntax_exact",
            "confidence_score": 1.0,
            "edge_kind": "calls"
        })
    }

    fn response_json(response: JsonRpcResponse) -> Value {
        let text = response
            .result
            .unwrap_or_else(|| panic!("success result: {:?}", response.error))["content"][0]
            ["text"]
            .as_str()
            .expect("content text")
            .to_string();
        serde_json::from_str(&text).expect("JSON content")
    }

    fn assert_unavailable_freshness_metadata(body: &Value) {
        assert_eq!(body.get("graph_built_at"), Some(&Value::Null));
        assert_eq!(body.get("indexed_head_oid"), Some(&Value::Null));
        assert_eq!(body.get("worktree_head_oid"), Some(&Value::Null));
        assert_eq!(body.get("worktree_dirty"), Some(&Value::Null));
        assert_eq!(body.get("response_file_oids_match"), Some(&Value::Null));
    }

    #[test]
    fn file_oid_match_reports_true_false_and_unknown() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/lib.rs"), "fn demo() {}\n").expect("write source");
        let indexed_oid = super::current_file_oid(dir.path(), "src/lib.rs")
            .expect("read current oid")
            .expect("current oid");

        assert_eq!(
            super::file_oid_cache::file_oid_match(
                dir.path(),
                "head-a",
                "graph-a",
                "src/lib.rs",
                &indexed_oid,
            ),
            Some(true)
        );
        assert_eq!(
            super::file_oid_cache::file_oid_match(
                dir.path(),
                "head-a",
                "graph-a",
                "src/lib.rs",
                "0000000000000000000000000000000000000000",
            ),
            Some(false)
        );
        assert_eq!(
            super::file_oid_cache::file_oid_match(
                dir.path(),
                "head-a",
                "graph-a",
                "src/missing.rs",
                &indexed_oid,
            ),
            None
        );
    }

    #[test]
    fn file_oid_match_toctou_race_returns_unknown() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/lib.rs"), "fn demo() {}\n").expect("write source");
        let indexed_oid = super::current_file_oid(dir.path(), "src/lib.rs")
            .expect("read current oid")
            .expect("current oid");

        let append_after_first_stat = |path: &std::path::Path| {
            use std::io::Write;

            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .expect("open source");
            file.write_all(b"// raced\n").expect("append source");
        };

        assert_eq!(
            super::file_oid_cache::file_oid_match_after_first_stat(
                dir.path(),
                "head-race",
                "graph-race",
                "src/lib.rs",
                &indexed_oid,
                &append_after_first_stat,
            ),
            None
        );
    }

    #[test]
    fn aggregate_file_oids_match_combines_file_results() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").expect("write a");
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").expect("write b");
        let a_oid = super::current_file_oid(dir.path(), "src/a.rs")
            .expect("read a oid")
            .expect("a oid");
        let b_oid = super::current_file_oid(dir.path(), "src/b.rs")
            .expect("read b oid")
            .expect("b oid");

        assert_eq!(
            super::file_oid_cache::aggregate_file_oids_match(
                dir.path(),
                "head-b",
                "graph-b",
                &[("src/a.rs", a_oid.as_str()), ("src/b.rs", b_oid.as_str())],
            ),
            Some(true)
        );
        assert_eq!(
            super::file_oid_cache::aggregate_file_oids_match(
                dir.path(),
                "head-b",
                "graph-b",
                &[
                    ("src/a.rs", a_oid.as_str()),
                    ("src/b.rs", "1111111111111111111111111111111111111111"),
                ],
            ),
            Some(false)
        );
        assert_eq!(
            super::file_oid_cache::aggregate_file_oids_match(
                dir.path(),
                "head-b",
                "graph-b",
                &[
                    ("src/a.rs", a_oid.as_str()),
                    ("src/missing.rs", b_oid.as_str())
                ],
            ),
            None
        );
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|error| panic!("run git {}: {error}", args.join(" ")));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git stdout utf8")
            .trim()
            .to_string()
    }

    fn init_clean_git_fixture(dir: &std::path::Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@spur"]);
        git(dir, &["config", "user.name", "Spur Test"]);
        std::fs::create_dir_all(dir.join("src")).expect("create src");
        std::fs::create_dir_all(dir.join("crates")).expect("create crates");
        std::fs::write(dir.join("src/root.rs"), numbered_fixture_source("root", 90))
            .expect("write root source");
        std::fs::write(
            dir.join("src/caller.rs"),
            numbered_fixture_source("caller", 20),
        )
        .expect("write caller source");
        std::fs::write(
            dir.join("src/callee.rs"),
            numbered_fixture_source("callee", 70),
        )
        .expect("write callee source");
        std::fs::write(
            dir.join("src/search.rs"),
            numbered_fixture_source("search", 90),
        )
        .expect("write search source");
        std::fs::write(dir.join("crates/foo"), numbered_fixture_source("foo", 40))
            .expect("write foo source");
        std::fs::write(
            dir.join("crates/other"),
            numbered_fixture_source("other", 50),
        )
        .expect("write other source");
        std::fs::write(dir.join(".git/info/exclude"), ".spur/\n").expect("ignore graph sidecar");
        git(
            dir,
            &[
                "add",
                "src/root.rs",
                "src/caller.rs",
                "src/callee.rs",
                "src/search.rs",
                "crates/foo",
                "crates/other",
            ],
        );
        git(dir, &["commit", "-q", "-m", "initial"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn numbered_fixture_source(label: &str, lines: usize) -> String {
        (1..=lines)
            .map(|line| format!("// {label} line {line}\n"))
            .collect()
    }

    fn write_fixture_pointer(dir: &TempDir, indexed_head_oid: &str) {
        let pointer_path = dir.path().join(".spur/graph-index.pointer.json");
        std::fs::create_dir_all(pointer_path.parent().expect("pointer parent"))
            .expect("create pointer parent");
        std::fs::write(
            pointer_path,
            serde_json::to_string_pretty(&json!({
                "schema": "spur-graph-pointer-v1",
                "graph_content_hash": "test",
                "manifest_version": "test",
                "source_kind": "git",
                "indexed_commit_oid": indexed_head_oid,
                "canonical_artifact_path": dir.path().join(".spur/graph-index.json")
            }))
            .expect("encode pointer"),
        )
        .expect("write pointer");
    }

    fn write_fixture_artifact_with_file_manifests(dir: &TempDir) {
        write_fixture_artifact(dir);
        let artifact_path = dir.path().join(".spur/graph-index.json");
        let mut artifact: Value =
            serde_json::from_slice(&std::fs::read(&artifact_path).expect("read fixture artifact"))
                .expect("parse fixture artifact");
        let files = artifact["files"]
            .as_array()
            .expect("fixture files")
            .iter()
            .map(|file| {
                let path = file["file_path"].as_str().expect("file path");
                let content_oid = super::current_file_oid(dir.path(), path)
                    .expect("read fixture file oid")
                    .expect("fixture file oid");
                json!({
                    "stable_file_id": file["stable_file_id"],
                    "path": path,
                    "content_oid": content_oid,
                    "node_ids": []
                })
            })
            .collect::<Vec<_>>();
        artifact["file_manifests"] = Value::Array(files);
        std::fs::write(
            artifact_path,
            serde_json::to_string_pretty(&artifact).expect("encode fixture artifact"),
        )
        .expect("write fixture artifact");
    }

    struct HandlerCase {
        name: &'static str,
        returned_file: &'static str,
        request: fn(
            &McpCallbackServer,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + '_>>,
    }

    fn response_file_oid_handler_cases() -> Vec<HandlerCase> {
        vec![
            HandlerCase {
                name: "code_callers",
                returned_file: "src/caller.rs",
                request: |server| {
                    Box::pin(async move {
                        response_json(
                            server
                                .handle_code_callers(
                                    Value::from(1),
                                    json!({ "symbol": "graph://symbol/root" }),
                                )
                                .await,
                        )
                    })
                },
            },
            HandlerCase {
                name: "code_search",
                returned_file: "src/search.rs",
                request: |server| {
                    Box::pin(async move {
                        response_json(
                            server
                                .handle_code_search(
                                    Value::from(1),
                                    json!({
                                        "query": "submit",
                                        "mode": "prefix",
                                        "symbol_kind": "function",
                                        "file": "src/search.rs",
                                        "limit": 10
                                    }),
                                )
                                .await,
                        )
                    })
                },
            },
            HandlerCase {
                name: "code_read_symbol",
                returned_file: "src/root.rs",
                request: |server| {
                    Box::pin(async move {
                        response_json(
                            server
                                .handle_code_read_symbol(
                                    Value::from(1),
                                    json!({ "stable_symbol_id": "graph://symbol/root" }),
                                )
                                .await,
                        )
                    })
                },
            },
        ]
    }

    fn setup_file_oid_metadata_fixture() -> (TempDir, String) {
        let dir = TempDir::new().expect("tempdir");
        let indexed_head_oid = init_clean_git_fixture(dir.path());
        write_fixture_artifact_with_file_manifests(&dir);
        write_fixture_pointer(&dir, &indexed_head_oid);
        (dir, indexed_head_oid)
    }

    fn append_to_fixture_file(dir: &TempDir, rel_path: &str, content: &str) {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(rel_path))
            .unwrap_or_else(|error| panic!("open {rel_path}: {error}"));
        file.write_all(content.as_bytes())
            .unwrap_or_else(|error| panic!("write {rel_path}: {error}"));
    }

    async fn assert_response_file_oids_match(
        case: &HandlerCase,
        mutate: impl FnOnce(&TempDir),
        expected: bool,
    ) {
        let _lock = CWD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _budget = (!expected).then(|| {
            super::set_graph_rebuild_latency_budget_for_test(std::time::Duration::from_millis(0))
        });
        let (dir, indexed_head_oid) = setup_file_oid_metadata_fixture();
        mutate(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = (case.request)(&server).await;

        assert_eq!(
            body["indexed_head_oid"], indexed_head_oid,
            "{} indexed head",
            case.name
        );
        assert_eq!(
            body["worktree_head_oid"], indexed_head_oid,
            "{} worktree head",
            case.name
        );
        assert_eq!(
            body["response_file_oids_match"], expected,
            "{} response_file_oids_match",
            case.name
        );
    }

    #[tokio::test]
    async fn response_file_oids_match_clean_worktree_returns_true() {
        for case in response_file_oid_handler_cases() {
            assert_response_file_oids_match(&case, |_| {}, true).await;
        }
    }

    #[tokio::test]
    async fn response_file_oids_match_edit_to_returned_file_returns_false() {
        for case in response_file_oid_handler_cases() {
            assert_response_file_oids_match(
                &case,
                |dir| append_to_fixture_file(dir, case.returned_file, "// returned edit\n"),
                false,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn response_file_oids_match_edit_to_unrelated_file_returns_true() {
        for case in response_file_oid_handler_cases() {
            assert_response_file_oids_match(
                &case,
                |dir| append_to_fixture_file(dir, "src/callee.rs", "// unrelated edit\n"),
                true,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn response_file_oids_match_untracked_file_does_not_flip() {
        for case in response_file_oid_handler_cases() {
            assert_response_file_oids_match(
                &case,
                |dir| {
                    std::fs::write(dir.path().join("scratch.log"), "untracked\n")
                        .expect("write untracked");
                },
                true,
            )
            .await;
        }
    }

    #[test]
    fn validate_file_path_arg_requires_slash_normalized_relative_paths() {
        assert_eq!(
            super::validate_file_path_arg("src/lib.rs").expect("valid relative file path"),
            "src/lib.rs"
        );

        for file in ["./src/lib.rs", "src/./lib.rs", "../src/lib.rs", "/abs/path"] {
            let error = match super::validate_file_path_arg(file) {
                Ok(_) => panic!("`{file}` must be rejected"),
                Err(error) => error,
            };
            assert_eq!(error.json_rpc_code(), -32602);
        }
    }

    #[tokio::test]
    async fn file_and_file_glob_mutually_exclusive_in_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "submit",
                    "file": "src/search.rs",
                    "file_glob": "src/*.rs"
                }),
            )
            .await;

        let error = response.error.expect("mutually exclusive error");
        assert_eq!(error.code, -32602);
        assert!(error
            .message
            .contains("fields 'file' and 'file_glob' are mutually exclusive"));
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_content_hash"],
            "test"
        );
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
    }

    #[tokio::test]
    async fn empty_query_rejected_by_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(Value::from(1), json!({ "query": " \n\t " }))
            .await;

        let error = response.error.expect("empty query error");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("field 'query' must not be empty"));
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
    }

    #[tokio::test]
    async fn absolute_or_dotdot_file_rejected_by_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        for args in [
            json!({ "query": "submit", "file": "/abs/path.rs" }),
            json!({ "query": "submit", "file": "../src/search.rs" }),
            json!({ "query": "submit", "file_glob": "/abs/*.rs" }),
            json!({ "query": "submit", "file_glob": "../src/*.rs" }),
        ] {
            let response = server.handle_code_search(Value::from(1), args).await;
            let error = response.error.expect("invalid path error");
            assert_eq!(error.code, -32602);
            assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
        }
    }

    #[tokio::test]
    async fn code_file_symbols_reads_parquet_artifact_from_current_pointer() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_current_parquet_fixture(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_file_symbols(Value::from(1), json!({ "file": "src/parquet.rs" }))
            .await;
        let body = response_json(response);
        let symbols = body["symbols"].as_array().expect("symbols");

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["selector"], "src/parquet.rs::parquet_root");
        assert_eq!(symbols[0]["uri"], "graph://symbol/parquet-root");
        assert_eq!(symbols[1]["selector"], "src/parquet.rs::parquet_child");
        assert_eq!(symbols[1]["uri"], "graph://symbol/parquet-child");
        assert_eq!(body["graph_content_hash"], "parquet-handler-test");
        assert_eq!(body["graph_index_version"], "parquet-test");
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_search_returns_ranked_candidates_with_freshness_metadata() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "sub",
                    "mode": "prefix",
                    "symbol_kind": "function",
                    "file": "src/search.rs",
                    "limit": 10
                }),
            )
            .await;
        let body = response_json(response);
        let candidates = body["candidates"].as_array().expect("candidates");

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0]["selector"], "src/search.rs::submit");
        assert_eq!(candidates[0]["uri"], "graph://symbol/search-submit");
        assert_eq!(candidates[0]["id"], "search-submit");
        assert_eq!(candidates[0]["entity_name"], "submit");
        assert_eq!(candidates[0]["qualified_name"], "submit");
        assert_eq!(candidates[0]["file_path"], "src/search.rs");
        assert_eq!(candidates[0]["line_range"], json!([70, 72]));
        assert_eq!(candidates[0]["symbol_kind"], "function");
        assert_eq!(candidates[1]["entity_name"], "submit_plan");
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_search_echoes_inputs_and_total_matches() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "submit",
                    "mode": "substring",
                    "symbol_kind": "function",
                    "file_glob": "src/*.rs",
                    "limit": 1
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["query"], "submit");
        assert_eq!(body["mode"], "substring");
        assert_eq!(body["symbol_kind"], "function");
        assert_eq!(body["file"], Value::Null);
        assert_eq!(body["file_glob"], "src/*.rs");
        assert_eq!(body["limit"], 1);
        assert_eq!(body["total_matches"], 2);
        assert_eq!(body["truncated"], true);
        assert_eq!(body["candidates"].as_array().expect("candidates").len(), 1);
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callers_returns_lightweight_symbol_rows() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
            .await;
        let body = response_json(response);

        let callers = body["callers"].as_array().expect("callers");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["uri"], "graph://symbol/caller");
        assert_eq!(body["callers"][0]["resolved"], true);
        assert_eq!(body["callers"][0]["edge_kind"], "calls");
        assert_eq!(body["callers"][0]["entity_name"], "call_root");
        assert_eq!(body["callers"][0]["file_path"], "src/caller.rs");
        assert_eq!(body["callers"][0]["line_range"], json!([3, 5]));
        assert_eq!(body["callers"][0]["symbol_kind"], "function");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 2,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 0,
                "unresolved": 1
            })
        );
        assert_eq!(body["unresolved_sample"], json!(["root"]));
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callers_can_include_unresolved_rows_when_requested() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/root",
                    "include_unresolved": true
                }),
            )
            .await;
        let body = response_json(response);
        let callers = body["callers"].as_array().expect("callers");

        assert_eq!(callers.len(), 2);
        assert_eq!(callers[0]["resolved"], true);
        assert_eq!(callers[1]["resolved"], false);
        assert_eq!(callers[1]["uri"], "graph://symbol/unresolved-caller");
        assert_eq!(callers[1]["target_label"], "root");
        assert_eq!(body["include_unresolved"], true);
        assert_eq!(body["unresolved_sample"], json!(["root"]));
    }

    #[tokio::test]
    async fn code_callers_counts_legacy_references_edges_as_references_other() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": {
                    "graph_index_version": GRAPH_INDEX_VERSION_TEMPORAL
                },
                "manifest_version": "test",
                "graph_content_hash": "test",
                "files": [
                    { "stable_file_id": "file-src-lib", "file_path": "src/lib.rs" }
                ],
                "symbols": [
                    symbol("caller", "src/lib.rs", [1, 1], "caller", "caller"),
                    symbol("root", "src/lib.rs", [3, 3], "root", "root")
                ],
                "edges": [
                    {
                        "source_stable_symbol_id": "caller",
                        "target_stable_symbol_id": "root",
                        "target_label": "root",
                        "relation": "references",
                        "confidence": "syntax_exact",
                        "confidence_score": 1.0
                    }
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "root" }))
                .await,
        );

        assert_eq!(body["callers"][0]["edge_kind"], "references_other");
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 0,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 1,
                "unresolved": 0
            })
        );
    }

    #[tokio::test]
    async fn code_callees_accepts_bare_symbol_id() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(Value::from(1), json!({ "symbol": "root" }))
            .await;
        let body = response_json(response);

        assert_eq!(body["callees"].as_array().expect("callees").len(), 3);
        assert_eq!(body["callees"][0]["resolved"], true);
        assert_eq!(body["callees"][0]["edge_kind"], "calls");
        assert_eq!(body["callees"][0]["uri"], "graph://symbol/callee");
        assert_eq!(body["callees"][0]["entity_name"], "callee");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 1,
                "calls_dyn": 1,
                "references_hof": 1,
                "references_other": 0,
                "unresolved": 0
            })
        );
        assert_eq!(body["unresolved_sample"], json!([]));
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callees_filters_unresolved_by_default_and_reports_counts() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(
                Value::from(1),
                json!({ "symbol": "graph://symbol/mixed-root" }),
            )
            .await;
        let body = response_json(response);
        let callees = body["callees"].as_array().expect("callees");

        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["resolved"], true);
        assert_eq!(callees[0]["edge_kind"], "calls");
        assert_eq!(callees[0]["uri"], "graph://symbol/mixed-callee");
        assert_eq!(callees[0]["entity_name"], "mixed_callee");
        assert_eq!(callees[0]["file_path"], "src/callee.rs");
        assert_eq!(callees[0]["line_range"], json!([60, 62]));
        assert_eq!(callees[0]["symbol_kind"], "function");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 2,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 0,
                "unresolved": 1
            })
        );
        assert_eq!(body["unresolved_sample"], json!(["into"]));

        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callees_can_include_unresolved_rows_when_requested() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/mixed-root",
                    "include_unresolved": true
                }),
            )
            .await;
        let body = response_json(response);
        let callees = body["callees"].as_array().expect("callees");

        assert_eq!(callees.len(), 2);
        assert_eq!(callees[1]["resolved"], false);
        assert_eq!(callees[1]["edge_kind"], "calls");
        assert_eq!(callees[1]["entity_name"], "into");
        assert_eq!(callees[1]["target_label"], "into");
        assert!(callees[1].get("uri").is_none());
        assert!(callees[1].get("file_path").is_none());
        assert_eq!(body["include_unresolved"], true);
        assert_eq!(body["unresolved_sample"], json!(["into"]));
    }

    #[tokio::test]
    async fn code_graph_handlers_resolve_selector_for_callers_and_callees() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let callers = response_json(
            server
                .handle_code_callers(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run" }),
                )
                .await,
        );
        assert_eq!(callers["callers"].as_array().expect("callers").len(), 1);
        assert_eq!(callers["callers"][0]["uri"], "graph://symbol/cache-caller");
        assert_eq!(callers["graph_content_hash"], "test");
        assert_eq!(callers["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&callers);

        let callees = response_json(
            server
                .handle_code_callees(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run" }),
                )
                .await,
        );
        assert_eq!(callees["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(callees["callees"][0]["resolved"], true);
        assert_eq!(callees["callees"][0]["edge_kind"], "calls");
        assert_eq!(callees["callees"][0]["uri"], "graph://symbol/cache-callee");
        assert_eq!(callees["graph_content_hash"], "test");
        assert_eq!(callees["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&callees);
    }

    #[tokio::test]
    async fn selector_takes_precedence_over_legacy_symbol_when_both_are_present() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_callees(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run", "symbol": "root" }),
                )
                .await,
        );

        assert_eq!(body["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(body["callees"][0]["uri"], "graph://symbol/cache-callee");
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn ambiguous_selector_defaults_to_successful_candidates_response() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "selector": "run" }))
            .await;
        assert!(
            response.error.is_none(),
            "ambiguous default must be success"
        );
        let body = response_json(response);

        assert_eq!(body["ambiguous"], true);
        assert_eq!(body["candidates"].as_array().expect("candidates").len(), 2);
        assert_eq!(body["candidates"][0]["selector"], "crates/foo::Cache::run");
        assert_eq!(
            body["candidates"][1]["selector"],
            "crates/other::Other::run"
        );
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn ambiguous_selector_can_be_returned_as_json_rpc_error() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(
                Value::from(1),
                json!({ "selector": "run", "on_ambiguous": "error" }),
            )
            .await;

        let error = response.error.expect("ambiguous error");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("selector `run` is ambiguous"));
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_content_hash"],
            "test"
        );
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_index_version"],
            GRAPH_INDEX_VERSION_TEMPORAL
        );
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
    }

    #[tokio::test]
    async fn code_subgraph_returns_json_and_clamps_radius_metadata() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "root", "radius": 9, "edge_kinds": ["calls"] }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 3);
        let edges = body["edges"].as_array().expect("edges");
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|edge| edge["edge_kind"] == "calls"));
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(body["metadata"]["radius"], 3);
        assert_eq!(
            body["metadata"]["warning"],
            "radius 9 exceeds max 3; clamped to 3"
        );
        assert_eq!(body["metadata"]["max_nodes"], 40);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_subgraph_enforces_default_node_budget() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_wide_subgraph_artifact(&dir, 45, 45);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "graph://symbol/wide-root", "radius": 1 }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 40);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], true);
        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 40);
        assert_eq!(body["edges"].as_array().expect("edges").len(), 39);
        assert_eq!(
            body["truncated_frontier"],
            json!([
                "wide-child-039",
                "wide-child-040",
                "wide-child-041",
                "wide-child-042",
                "wide-child-043",
                "wide-child-044"
            ])
        );
    }

    #[tokio::test]
    async fn code_subgraph_enforces_default_edge_budget() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_wide_subgraph_artifact(&dir, 130, 130);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/wide-root",
                    "radius": 1,
                    "max_nodes": 400
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 400);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], true);
        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 121);
        assert_eq!(body["edges"].as_array().expect("edges").len(), 120);
        assert_eq!(
            body["truncated_frontier"],
            json!([
                "wide-child-120",
                "wide-child-121",
                "wide-child-122",
                "wide-child-123",
                "wide-child-124",
                "wide-child-125",
                "wide-child-126",
                "wide-child-127",
                "wide-child-128",
                "wide-child-129"
            ])
        );
    }

    #[tokio::test]
    async fn code_subgraph_clamps_and_echoes_requested_budgets() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({
                    "symbol": "root",
                    "radius": 1,
                    "max_nodes": 999,
                    "max_edges": 9999
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 400);
        assert_eq!(body["metadata"]["max_edges"], 1200);
        assert_eq!(body["metadata"]["requested_max_nodes"], 999);
        assert_eq!(body["metadata"]["requested_max_edges"], 9999);
        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
    }

    #[tokio::test]
    async fn code_subgraph_returns_empty_frontier_when_untruncated() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(Value::from(1), json!({ "symbol": "root", "radius": 1 }))
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
    }

    #[tokio::test]
    async fn code_subgraph_filters_unresolved_by_default_and_can_include_them() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let default_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/mixed-root",
                        "radius": 1,
                        "edge_kinds": ["calls"]
                    }),
                )
                .await,
        );
        let default_edges = default_body["edges"].as_array().expect("default edges");
        assert_eq!(default_edges.len(), 1);
        assert!(default_edges
            .iter()
            .all(|edge| edge["target_uri"].as_str().is_some()));
        assert_eq!(default_body["include_unresolved"], false);

        let included_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/mixed-root",
                        "radius": 1,
                        "edge_kinds": ["calls"],
                        "include_unresolved": true
                    }),
                )
                .await,
        );
        let included_edges = included_body["edges"].as_array().expect("included edges");
        assert_eq!(included_edges.len(), 2);
        assert!(included_edges
            .iter()
            .any(|edge| edge["target_label"] == "into" && edge["target_uri"].is_null()));
        assert_eq!(included_body["include_unresolved"], true);
    }

    #[tokio::test]
    async fn code_subgraph_can_include_incoming_unresolved_caller_edges() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/root",
                        "radius": 1,
                        "edge_kinds": ["calls"],
                        "include_unresolved": true
                    }),
                )
                .await,
        );
        let edges = body["edges"].as_array().expect("edges");

        assert!(edges.iter().any(|edge| {
            edge["source_uri"] == "graph://symbol/unresolved-caller"
                && edge["target_uri"].is_null()
                && edge["target_label"] == "root"
                && edge["resolved"] == false
        }));
    }

    #[tokio::test]
    async fn code_subgraph_edge_kinds_accept_public_edge_kind_values() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let dyn_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({ "symbol": "root", "radius": 1, "edge_kinds": ["calls_dyn"] }),
                )
                .await,
        );
        let dyn_edges = dyn_body["edges"].as_array().expect("dyn edges");
        assert_eq!(dyn_edges.len(), 1);
        assert_eq!(dyn_edges[0]["edge_kind"], "calls_dyn");

        let hof_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({ "symbol": "root", "radius": 1, "edge_kinds": ["references_hof"] }),
                )
                .await,
        );
        let hof_edges = hof_body["edges"].as_array().expect("hof edges");
        assert_eq!(hof_edges.len(), 1);
        assert_eq!(hof_edges[0]["edge_kind"], "references_hof");
    }

    #[tokio::test]
    async fn code_subgraph_returns_mermaid_output() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "root", "radius": 1, "format": "mermaid" }),
            )
            .await;
        let body = response_json(response);
        let mermaid = body["mermaid"].as_str().expect("mermaid");

        assert!(mermaid.starts_with("graph TD\n"));
        assert!(mermaid.contains("root[\"root\"]"));
        assert!(mermaid.contains("root --> callee"));
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], GRAPH_INDEX_VERSION_TEMPORAL);
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn graph_metadata_reports_pointer_head_and_dirty_state() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        let indexed_head_oid = init_clean_git_fixture(dir.path());
        write_fixture_artifact(&dir);
        write_fixture_pointer(&dir, &indexed_head_oid);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let clean = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
                .await,
        );

        assert!(
            chrono::DateTime::parse_from_rfc3339(
                clean["graph_built_at"].as_str().expect("graph_built_at")
            )
            .is_ok(),
            "graph_built_at must be RFC3339"
        );
        assert_eq!(clean["indexed_head_oid"], indexed_head_oid);
        assert_eq!(clean["worktree_head_oid"], indexed_head_oid);
        assert_eq!(clean["worktree_dirty"], false);

        std::fs::write(
            dir.path().join("src/root.rs"),
            "fn root() { let _dirty = true; }\n",
        )
        .expect("dirty source");

        let dirty = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
                .await,
        );

        assert_eq!(dirty["indexed_head_oid"], indexed_head_oid);
        assert_eq!(dirty["worktree_head_oid"], indexed_head_oid);
        assert_eq!(dirty["worktree_dirty"], true);
    }

    #[tokio::test]
    async fn graph_metadata_ignores_untracked_files_when_reporting_dirty() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        let indexed_head_oid = init_clean_git_fixture(dir.path());
        write_fixture_artifact(&dir);
        write_fixture_pointer(&dir, &indexed_head_oid);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        std::fs::write(dir.path().join("scratch.log"), "untracked\n")
            .expect("write untracked file");

        let body = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
                .await,
        );

        assert_eq!(body["indexed_head_oid"], indexed_head_oid);
        assert_eq!(body["worktree_head_oid"], indexed_head_oid);
        assert_eq!(
            body["worktree_dirty"], false,
            "untracked-only worktrees must not flip worktree_dirty"
        );
    }

    #[tokio::test]
    async fn missing_artifact_returns_clear_json_rpc_error() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("create .git marker");
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "symbol": "root" }))
            .await;

        let error = response.error.expect("error");
        assert!(error
            .message
            .contains("graph artifact not found; run `spur graph build`"));
        assert!(error
            .message
            .contains(dir.path().to_string_lossy().as_ref()));
        assert!(
            error.data.is_none(),
            "artifact-load failures must not echo graph metadata"
        );
    }
}
