use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::mentions::entry::{CodeMentionCandidate, MentionEntry, MentionKind, MentionSource};
use spur_graph::{
    load_artifact_slim, read_artifact_header_parquet, resolve_artifact_location,
    CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints, CodeMentionKind,
    CodeMentionPayload, CodeMentionValidationSpec, GraphFileArtifact, GraphIndexArtifact,
    GraphQueryClient, GraphSymbolArtifact, ParquetClient, ResolvedArtifact, SearchFilters,
    SearchMode, SearchOptions, SearchSymbol, CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX,
};

struct CodeGraphCacheKey {
    path: PathBuf,
    mtime: Option<SystemTime>,
    len: Option<u64>,
    content_hash: Option<String>,
}

pub struct CodeGraphMentionSource {
    worktree_root: Option<PathBuf>,
    explicit_override: Option<PathBuf>,
    cache_key: Option<CodeGraphCacheKey>,
    cached_entries: Vec<MentionEntry>,
    payloads: Vec<(String, Arc<CodeMentionPayload>)>,
    candidates: Arc<Vec<CodeMentionCandidate>>,
    payload_backend: Option<CodePayloadBackend>,
    #[cfg(test)]
    reload_count: usize,
}

enum CodePayloadBackend {
    Parquet {
        client: Box<ParquetClient>,
        graph_index_version: String,
    },
    InMemory {
        symbols: HashMap<String, GraphSymbolArtifact>,
        graph_index_version: String,
    },
}

impl CodeGraphMentionSource {
    pub fn new(artifact_path: impl Into<PathBuf>) -> Self {
        Self {
            worktree_root: None,
            explicit_override: Some(artifact_path.into()),
            cache_key: None,
            cached_entries: Vec::new(),
            payloads: Vec::new(),
            candidates: Arc::default(),
            payload_backend: None,
            #[cfg(test)]
            reload_count: 0,
        }
    }

    pub(crate) fn for_worktree(
        worktree_root: impl Into<PathBuf>,
        explicit_override: Option<PathBuf>,
    ) -> Self {
        Self {
            worktree_root: Some(worktree_root.into()),
            explicit_override,
            cache_key: None,
            cached_entries: Vec::new(),
            payloads: Vec::new(),
            candidates: Arc::default(),
            payload_backend: None,
            #[cfg(test)]
            reload_count: 0,
        }
    }

    fn clear_loaded_state(&mut self) {
        self.cache_key = None;
        self.cached_entries.clear();
        self.payloads.clear();
        self.candidates = Arc::default();
        self.payload_backend = None;
    }
}

impl MentionSource for CodeGraphMentionSource {
    fn name(&self) -> &'static str {
        "code_graph"
    }

    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let worktree_root = self
            .worktree_root
            .clone()
            .unwrap_or_else(|| spur_graph::resolve_worktree_root_from(cwd.to_path_buf()));
        let resolved =
            match resolve_artifact_location(&worktree_root, self.explicit_override.as_deref()) {
                Ok(resolved) => resolved,
                Err(error) => {
                    let explicit_override = self
                        .explicit_override
                        .as_ref()
                        .map(|path| path.display().to_string());
                    tracing::warn!(
                        error = %error,
                        worktree_root = %worktree_root.display(),
                        explicit_override = explicit_override.as_deref(),
                        "code graph mention source disabled; no readable artifact found"
                    );
                    self.clear_loaded_state();
                    return Ok(Vec::new());
                }
            };
        let metadata = fs::metadata(&resolved.path).ok();
        let modified = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok());
        let len = metadata.as_ref().map(|metadata| metadata.len());

        if let Some(cache_key) = self.cache_key.as_mut() {
            if cache_key.path == resolved.path {
                if let Ok(current_hash) = read_resolved_content_hash(&resolved) {
                    if let (Some(cached_hash), Some(current_hash)) =
                        (cache_key.content_hash.as_deref(), current_hash.as_deref())
                    {
                        if cached_hash == current_hash {
                            cache_key.path = resolved.path.clone();
                            cache_key.mtime = modified;
                            cache_key.len = len;
                            let span = tracing::debug_span!(
                                "code_graph_build",
                                path = %resolved.path.display(),
                                cached = "hash"
                            );
                            let _guard = span.enter();
                            tracing::debug!(
                                path = %resolved.path.display(),
                                "code graph mention source cached by content hash"
                            );
                            return Ok(self.cached_entries.clone());
                        }
                    }
                }
            }
        }

        let span = tracing::debug_span!(
            "code_graph_build",
            path = %resolved.path.display(),
            cached = "miss"
        );
        let _guard = span.enter();

        let loaded = match tracing::debug_span!("load_code_mention_index")
            .in_scope(|| load_code_index(&resolved.path))
        {
            Ok(loaded) => loaded,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %resolved.path.display(),
                    "code graph mention source disabled for unreadable artifact"
                );
                self.clear_loaded_state();
                return Ok(Vec::new());
            }
        };

        #[cfg(test)]
        {
            self.reload_count += 1;
        }

        let content_hash = Some(loaded.content_hash.clone());
        for diagnostic in &loaded.diagnostics {
            tracing::warn!(
                diagnostic = diagnostic.as_str(),
                path = %resolved.path.display(),
                "code graph mention source loaded artifact with diagnostic"
            );
        }

        let mut payloads = Vec::new();
        let entries = tracing::debug_span!("materialize_entries").in_scope(|| {
            file_entries_and_payloads(loaded.files, &loaded.graph_index_version, &mut payloads)
        });
        self.payloads = payloads;
        self.candidates = Arc::new(compact_candidates(loaded.candidates));
        self.payload_backend = Some(loaded.payload_backend);
        self.cached_entries = entries.clone();
        tracing::info!(
            path = %resolved.path.display(),
            entries = entries.len(),
            candidates = self.candidates.len(),
            payloads = self.payloads.len(),
            "code graph mention source reloaded"
        );
        self.cache_key = Some(CodeGraphCacheKey {
            path: resolved.path,
            mtime: modified,
            len,
            content_hash,
        });
        Ok(entries)
    }

    fn code_payloads(&self) -> &[(String, Arc<CodeMentionPayload>)] {
        &self.payloads
    }

    fn code_candidates(&self) -> Arc<Vec<CodeMentionCandidate>> {
        Arc::clone(&self.candidates)
    }

    fn hydrate_code_payloads(
        &self,
        stable_symbol_ids: &[String],
    ) -> anyhow::Result<Vec<(String, Arc<CodeMentionPayload>)>> {
        let Some(backend) = &self.payload_backend else {
            return Ok(Vec::new());
        };
        let (symbols, graph_index_version) = match backend {
            CodePayloadBackend::Parquet {
                client,
                graph_index_version,
            } => (
                client.symbols_by_stable_ids(stable_symbol_ids)?,
                graph_index_version.as_str(),
            ),
            CodePayloadBackend::InMemory {
                symbols,
                graph_index_version,
            } => (
                stable_symbol_ids
                    .iter()
                    .filter_map(|stable_id| symbols.get(stable_id).cloned())
                    .collect(),
                graph_index_version.as_str(),
            ),
        };
        Ok(symbols
            .into_iter()
            .map(|symbol| {
                let uri = format!("{}{}", CODE_SYMBOL_URI_PREFIX, symbol.stable_symbol_id);
                let display = symbol.entity_name.clone();
                let payload = symbol_payload(&symbol, &uri, &display, graph_index_version);
                (uri, Arc::new(payload))
            })
            .collect())
    }
}

fn read_resolved_content_hash(resolved: &ResolvedArtifact) -> anyhow::Result<Option<String>> {
    Ok(Some(
        read_artifact_header_parquet(&resolved.path)?.graph_content_hash,
    ))
}

struct LoadedCodeIndex {
    files: Vec<GraphFileArtifact>,
    candidates: Vec<SearchSymbol>,
    payload_backend: CodePayloadBackend,
    graph_index_version: String,
    content_hash: String,
    diagnostics: Vec<String>,
}

fn load_code_index(path: &Path) -> anyhow::Result<LoadedCodeIndex> {
    if path.is_dir() {
        let client = ParquetClient::open(path)?;
        let manifest = client.manifest();
        let graph_index_version = manifest.graph_index_version.clone();
        let content_hash = manifest.graph_content_hash.clone();
        let files = client.files()?;
        let diagnostics = client.diagnostics()?;
        let candidates = client
            .search_symbols(&SearchOptions {
                query: String::new(),
                mode: SearchMode::Substring,
                filters: SearchFilters::default(),
                limit: usize::MAX,
            })?
            .candidates;
        return Ok(LoadedCodeIndex {
            files,
            candidates,
            payload_backend: CodePayloadBackend::Parquet {
                client: Box::new(client),
                graph_index_version: graph_index_version.clone(),
            },
            graph_index_version,
            content_hash,
            diagnostics,
        });
    }

    let artifact: GraphIndexArtifact = load_artifact_slim(path)?;
    let graph_index_version = artifact.header.graph_index_version.clone();
    let content_hash = artifact.graph_content_hash.clone();
    let diagnostics = artifact.diagnostics.clone();
    let candidates = artifact.symbols.iter().map(SearchSymbol::from).collect();
    let symbols = artifact
        .symbols
        .into_iter()
        .map(|symbol| (symbol.stable_symbol_id.clone(), symbol))
        .collect();
    Ok(LoadedCodeIndex {
        files: artifact.files,
        candidates,
        payload_backend: CodePayloadBackend::InMemory {
            symbols,
            graph_index_version: graph_index_version.clone(),
        },
        graph_index_version,
        content_hash,
        diagnostics,
    })
}

fn compact_candidates(candidates: Vec<SearchSymbol>) -> Vec<CodeMentionCandidate> {
    let mut interned = HashMap::<String, Arc<str>>::new();
    let mut compact = candidates
        .into_iter()
        .map(|candidate| CodeMentionCandidate {
            stable_symbol_id: candidate.stable_symbol_id.into_boxed_str(),
            entity_name: candidate.entity_name.into_boxed_str(),
            file_path: intern(&mut interned, candidate.file_path),
            line_range: candidate.line_range,
            symbol_kind: intern(&mut interned, candidate.symbol_kind),
            enclosing_scope: candidate
                .enclosing_scope
                .map(|scope| intern(&mut interned, scope)),
        })
        .collect::<Vec<_>>();
    compact.sort_by(|left, right| {
        left.entity_name
            .len()
            .cmp(&right.entity_name.len())
            .then(left.entity_name.cmp(&right.entity_name))
            .then(left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });
    compact
}

fn intern(interned: &mut HashMap<String, Arc<str>>, value: String) -> Arc<str> {
    if let Some(existing) = interned.get(&value) {
        return Arc::clone(existing);
    }
    let shared = Arc::<str>::from(value.as_str());
    interned.insert(value, Arc::clone(&shared));
    shared
}

fn file_entries_and_payloads(
    files: Vec<GraphFileArtifact>,
    graph_index_version: &str,
    payloads: &mut Vec<(String, Arc<CodeMentionPayload>)>,
) -> Vec<MentionEntry> {
    let mut entries = Vec::with_capacity(files.len());

    for file in files {
        let uri = format!("{}{}", CODE_FILE_URI_PREFIX, file.stable_file_id);
        let display = file.file_path.clone();
        payloads.push((
            uri.clone(),
            Arc::new(file_payload(&file, &uri, &display, graph_index_version)),
        ));
        entries.push(MentionEntry {
            section_header: None,
            kind: MentionKind::CodeFile,
            uri,
            display,
            secondary: None,
            agent: None,
            model: None,
            effort: None,
            worker_kind: None,
            worker_cli_identity: None,
            code_path: Some(file.file_path.clone()),
            code_scope: None,
            tag: Some("file".to_string()),
            search_text: Some(file.file_path),
            atom_text: None,
            unconsumed_suffix: None,
            issue_preview: None,
        });
    }

    entries
}

pub(crate) fn entry_for_candidate(candidate: &CodeMentionCandidate) -> MentionEntry {
    let display = candidate.entity_name.to_string();
    let code_scope = candidate
        .enclosing_scope
        .as_ref()
        .map(|scope| scope.to_string());
    let atom_text = code_scope
        .as_ref()
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!("@{}::{}", scope, display));
    MentionEntry {
        section_header: None,
        kind: MentionKind::CodeSymbol,
        uri: format!("{}{}", CODE_SYMBOL_URI_PREFIX, candidate.stable_symbol_id),
        display: display.clone(),
        secondary: Some(symbol_secondary_fields(
            &display,
            candidate.file_path.as_ref(),
            candidate.line_range,
            candidate.symbol_kind.as_ref(),
            code_scope.as_deref(),
        )),
        agent: None,
        model: None,
        effort: None,
        worker_kind: None,
        worker_cli_identity: None,
        code_path: Some(candidate.file_path.to_string()),
        code_scope,
        tag: Some(format!("symbol:{}", candidate.symbol_kind)),
        search_text: None,
        atom_text,
        unconsumed_suffix: None,
        issue_preview: None,
    }
}

fn file_payload(
    file: &GraphFileArtifact,
    uri: &str,
    display: &str,
    graph_index_version: &str,
) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: CodeMentionAuthoritative {
            display: display.to_string(),
            uri: uri.to_string(),
            kind: CodeMentionKind::File,
            file_path: file.file_path.clone(),
            validation: CodeMentionValidationSpec::FileExists {
                path: file.file_path.clone(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: None,
            byte_range: None,
            symbol_kind: None,
            entity_name: None,
            qualified_name: String::new(),
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: graph_index_version.to_string(),
        },
    }
}

fn symbol_payload(
    symbol: &GraphSymbolArtifact,
    uri: &str,
    display: &str,
    graph_index_version: &str,
) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: CodeMentionAuthoritative {
            display: display.to_string(),
            uri: uri.to_string(),
            kind: CodeMentionKind::Symbol,
            file_path: symbol.file_path.clone(),
            validation: CodeMentionValidationSpec::SymbolRange {
                path: symbol.file_path.clone(),
                line_range: symbol.line_range,
                byte_range: symbol.byte_range,
                entity_name: symbol.entity_name.clone(),
                anchor_hash: symbol.anchor_hash.clone(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: Some(symbol.line_range),
            byte_range: Some(symbol.byte_range),
            symbol_kind: Some(symbol.symbol_kind.clone()),
            entity_name: Some(symbol.entity_name.clone()),
            qualified_name: symbol.qualified_name.clone(),
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: symbol.enclosing_scope.clone(),
            graph_index_version: graph_index_version.to_string(),
        },
    }
}

fn symbol_secondary_fields(
    entity_name: &str,
    file_path: &str,
    line_range: [usize; 2],
    symbol_kind: &str,
    enclosing_scope: Option<&str>,
) -> String {
    if let Some(scope) = enclosing_scope {
        format!(
            "{}::{} · {}:{} ({})",
            scope, entity_name, file_path, line_range[0], symbol_kind
        )
    } else {
        format!(
            "{} · {}:{} ({})",
            entity_name, file_path, line_range[0], symbol_kind
        )
    }
}

#[cfg(test)]
fn symbol_secondary(symbol: &GraphSymbolArtifact) -> String {
    symbol_secondary_fields(
        &symbol.entity_name,
        &symbol.file_path,
        symbol.line_range,
        &symbol.symbol_kind,
        symbol.enclosing_scope.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{set_file_mtime, FileTime};
    use spur_graph::{
        write_artifact_parquet, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
        GraphIndexHeader, NodeId, WriteOptions,
    };
    use std::fs;
    use std::time::Duration;

    fn write_fixture(path: &Path, file_path: &str) {
        let content_hash = format!("hash-{}", file_path.replace('/', "-"));
        write_fixture_with_hash(path, file_path, &content_hash);
    }

    fn write_fixture_with_hash(path: &Path, file_path: &str, content_hash: &str) {
        write_artifact_parquet(
            &graph_artifact("file-1", file_path, content_hash),
            path,
            WriteOptions::default(),
            Vec::new(),
        )
        .expect("write fixture");
    }

    fn rewrite_until_mtime_changes(path: &Path, file_path: &str, previous: SystemTime) {
        rewrite_until_mtime_changes_with(path, previous, || write_fixture(path, file_path));
    }

    fn rewrite_until_mtime_changes_with(
        path: &Path,
        previous: SystemTime,
        mut rewrite: impl FnMut(),
    ) {
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            rewrite();
            let current = fs::metadata(path)
                .expect("metadata")
                .modified()
                .expect("modified");
            if current != previous {
                return;
            }
        }
        panic!("artifact mtime did not change");
    }

    #[test]
    fn symbol_secondary_renders_scope_name_path_line_and_kind() {
        let scoped = GraphSymbolArtifact {
            stable_symbol_id: "symbol-cache-run".to_string(),
            file_path: "crates/example/src/cache.rs".to_string(),
            byte_range: [0, 20],
            line_range: [120, 145],
            entity_name: "run".to_string(),
            qualified_name: "Cache::run".to_string(),
            symbol_kind: "fn".to_string(),
            anchor_hash: "anchor-cache-run".to_string(),
            enclosing_scope: Some("Cache".to_string()),
        };
        assert_eq!(
            symbol_secondary(&scoped),
            "Cache::run · crates/example/src/cache.rs:120 (fn)"
        );

        let bare = GraphSymbolArtifact {
            stable_symbol_id: "symbol-cache".to_string(),
            file_path: "crates/example/src/cache.rs".to_string(),
            byte_range: [0, 20],
            line_range: [42, 88],
            entity_name: "Cache".to_string(),
            qualified_name: "Cache".to_string(),
            symbol_kind: "struct".to_string(),
            anchor_hash: "anchor-cache".to_string(),
            enclosing_scope: None,
        };
        assert_eq!(
            symbol_secondary(&bare),
            "Cache · crates/example/src/cache.rs:42 (struct)"
        );
    }

    #[test]
    fn build_uses_mtime_cache_and_reloads_on_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.parquet");
        write_fixture(&artifact_path, "src/main.rs");
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let second = source.build(Path::new(".")).expect("second build");
        assert_eq!(second.len(), 1);
        assert_eq!(source.reload_count, 1);

        let previous_mtime = fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes(&artifact_path, "src/lib.rs", previous_mtime);

        let third = source.build(Path::new(".")).expect("third build");
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].display, "src/lib.rs");
        assert_eq!(source.reload_count, 2);

        let stable_metadata = fs::metadata(&artifact_path).expect("metadata");
        let stable_mtime = stable_metadata.modified().expect("modified");
        write_fixture(&artifact_path, "src/lib_rewritten_with_longer_name.rs");
        let stable_filetime = FileTime::from_system_time(stable_mtime);
        set_file_mtime(&artifact_path, stable_filetime).expect("restore mtime");

        let fourth = source.build(Path::new(".")).expect("fourth build");
        assert_eq!(fourth.len(), 1);
        assert_eq!(fourth[0].display, "src/lib_rewritten_with_longer_name.rs");
        assert_eq!(source.reload_count, 3);
    }

    #[test]
    fn code_payloads_cache_hit_reuses_arc_instances() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.parquet");
        write_fixture(&artifact_path, "src/main.rs");
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let _ = source.build(Path::new(".")).expect("first build");
        let first = source.code_payloads();
        let (uri, first_payload) = first.first().expect("first payload");
        let uri = uri.clone();
        let first_payload = Arc::clone(first_payload);

        let _ = source.build(Path::new(".")).expect("second build");
        let second = source.code_payloads();
        let second_payload = second
            .iter()
            .find(|(candidate_uri, _)| candidate_uri == &uri)
            .map(|(_, payload)| payload)
            .expect("second payload");

        assert!(Arc::ptr_eq(&first_payload, second_payload));
    }

    #[test]
    fn parquet_build_keeps_symbols_compact_and_hydrates_only_selected_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut artifact = graph_artifact("file-1", "src/lib.rs", "hash-compact");
        artifact.symbols = vec![
            GraphSymbolArtifact {
                stable_symbol_id: "symbol-alpha".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                byte_range: [0, 10],
                line_range: [1, 1],
                entity_name: "alpha".to_owned(),
                qualified_name: "alpha".to_owned(),
                symbol_kind: "fn".to_owned(),
                anchor_hash: "anchor-alpha".to_owned(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "symbol-beta".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                byte_range: [11, 20],
                line_range: [2, 2],
                entity_name: "beta".to_owned(),
                qualified_name: "module::beta".to_owned(),
                symbol_kind: "fn".to_owned(),
                anchor_hash: "anchor-beta".to_owned(),
                enclosing_scope: Some("module".to_owned()),
            },
        ];
        artifact.symbol_node_ids = vec![NodeId(2), NodeId(3)];
        artifact.file_manifests[0].node_ids = vec![NodeId(1), NodeId(2), NodeId(3)];
        let parquet_dir =
            write_artifact_parquet(&artifact, temp.path(), WriteOptions::default(), Vec::new())
                .expect("write parquet artifact");
        let mut source = CodeGraphMentionSource::new(&parquet_dir);

        let entries = source.build(Path::new(".")).expect("build compact index");
        let candidates = source.code_candidates();
        let payloads = source
            .hydrate_code_payloads(&["symbol-beta".to_owned()])
            .expect("hydrate selected symbol");

        assert_eq!(
            entries.len(),
            1,
            "only the file row is materialized eagerly"
        );
        assert_eq!(entries[0].kind, MentionKind::CodeFile);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            source.code_payloads().len(),
            1,
            "only the file payload is eager"
        );
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].0, "graph://symbol/symbol-beta");
        assert_eq!(
            payloads[0].1.extraction_hints.qualified_name,
            "module::beta"
        );
        assert!(payloads
            .iter()
            .all(|(uri, _)| uri != "graph://symbol/symbol-alpha"));

        let _ = source.build(Path::new(".")).expect("reuse compact index");
        let cached_candidates = source.code_candidates();
        assert!(Arc::ptr_eq(&candidates, &cached_candidates));
        assert_eq!(source.reload_count, 1);
    }

    #[test]
    fn build_uses_content_hash_when_metadata_changes_but_content_does_not() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.parquet");
        write_fixture_with_hash(&artifact_path, "src/main.rs", "hash-same");
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let previous_mtime = fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes_with(&artifact_path, previous_mtime, || {
            write_fixture_with_hash(&artifact_path, "src/main.rs", "hash-same");
        });

        let second = source.build(Path::new(".")).expect("second build");
        assert_eq!(second.len(), 1);
        assert_eq!(source.reload_count, 1);
    }

    #[test]
    fn build_uses_parquet_manifest_hash_when_directory_metadata_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet_dir = write_artifact_parquet(
            &graph_artifact("file-1", "src/main.rs", "hash-same"),
            temp.path(),
            WriteOptions::default(),
            Vec::new(),
        )
        .expect("write parquet artifact");
        let mut source = CodeGraphMentionSource::new(&parquet_dir);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let previous_mtime = fs::metadata(&parquet_dir)
            .expect("metadata")
            .modified()
            .expect("modified");
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            let changed = previous_mtime + Duration::from_secs(1);
            set_file_mtime(&parquet_dir, FileTime::from_system_time(changed))
                .expect("update parquet dir mtime");
            let current = fs::metadata(&parquet_dir)
                .expect("metadata")
                .modified()
                .expect("modified");
            if current != previous_mtime {
                break;
            }
        }

        let second = source.build(Path::new(".")).expect("second build");
        assert_eq!(second.len(), 1);
        assert_eq!(source.reload_count, 1);
    }

    #[test]
    fn build_reloads_when_hash_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.parquet");
        write_fixture_with_hash(&artifact_path, "src/main.rs", "hash-before");
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let previous_mtime = fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes_with(&artifact_path, previous_mtime, || {
            write_fixture_with_hash(&artifact_path, "src/lib.rs", "hash-after");
        });

        let second = source.build(Path::new(".")).expect("second build");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].display, "src/lib.rs");
        assert_eq!(source.reload_count, 2);
    }

    fn graph_artifact(
        stable_file_id: &str,
        file_path: &str,
        content_hash: &str,
    ) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "source-test".to_string(),
                content_hash_blake3: Some(content_hash.to_string()),
            },
            manifest_version: "source-test-manifest".to_string(),
            graph_content_hash: content_hash.to_string(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: stable_file_id.to_string(),
                path: file_path.to_string(),
                content_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                node_ids: Vec::new(),
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: stable_file_id.to_string(),
                file_path: file_path.to_string(),
            }],
            file_node_ids: vec![NodeId(1)],
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
}
