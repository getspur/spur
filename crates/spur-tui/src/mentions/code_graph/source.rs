use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::mentions::entry::{MentionEntry, MentionKind, MentionSource};
use spur_graph::{
    load_artifact, read_artifact_header, read_artifact_header_parquet, resolve_artifact_location,
    ArtifactFormat, CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints,
    CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec, GraphFileArtifact,
    GraphIndexArtifact, GraphSymbolArtifact, ResolvedArtifact, CODE_FILE_URI_PREFIX,
    CODE_SYMBOL_URI_PREFIX,
};

struct CodeGraphCacheKey {
    path: PathBuf,
    format: ArtifactFormat,
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
    #[cfg(test)]
    reload_count: usize,
}

impl CodeGraphMentionSource {
    pub fn new(artifact_path: impl Into<PathBuf>) -> Self {
        Self {
            worktree_root: None,
            explicit_override: Some(artifact_path.into()),
            cache_key: None,
            cached_entries: Vec::new(),
            payloads: Vec::new(),
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
            #[cfg(test)]
            reload_count: 0,
        }
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
                    self.cache_key = None;
                    self.cached_entries.clear();
                    self.payloads.clear();
                    return Ok(Vec::new());
                }
            };
        let metadata = fs::metadata(&resolved.path).ok();
        let modified = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok());
        let len = metadata.as_ref().map(|metadata| metadata.len());
        let cached_fast = resolved.format == ArtifactFormat::LegacyJson
            && matches!(
                (&self.cache_key, modified, len),
                (
                    Some(CodeGraphCacheKey {
                        path,
                        format,
                        mtime: cached_mtime,
                        len: cached_len,
                        ..
                    }),
                    Some(current_mtime),
                    Some(current_len)
                )
                    if path == &resolved.path
                        && *format == resolved.format
                        && *cached_mtime == Some(current_mtime)
                        && *cached_len == Some(current_len)
            );
        if cached_fast {
            let span = tracing::debug_span!(
                "code_graph_build",
                path = %resolved.path.display(),
                cached = "fast"
            );
            let _guard = span.enter();
            return Ok(self.cached_entries.clone());
        }

        if let Some(cache_key) = self.cache_key.as_mut() {
            if cache_key.path == resolved.path && cache_key.format == resolved.format {
                if let Ok(current_hash) = read_resolved_content_hash(&resolved) {
                    if let (Some(cached_hash), Some(current_hash)) =
                        (cache_key.content_hash.as_deref(), current_hash.as_deref())
                    {
                        if cached_hash == current_hash {
                            cache_key.path = resolved.path.clone();
                            cache_key.format = resolved.format;
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

        let artifact = match tracing::debug_span!("load_artifact")
            .in_scope(|| load_artifact(&resolved.path))
        {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %resolved.path.display(),
                    "code graph mention source disabled for unreadable artifact"
                );
                self.cache_key = None;
                self.cached_entries.clear();
                self.payloads.clear();
                return Ok(Vec::new());
            }
        };

        #[cfg(test)]
        {
            self.reload_count += 1;
        }

        let content_hash = match resolved.format {
            ArtifactFormat::Parquet => Some(artifact.graph_content_hash.clone()),
            ArtifactFormat::LegacyJson => artifact.header.content_hash_blake3.clone(),
        };
        for diagnostic in &artifact.diagnostics {
            tracing::warn!(
                diagnostic = diagnostic.as_str(),
                path = %resolved.path.display(),
                "code graph mention source loaded artifact with diagnostic"
            );
        }

        let mut payloads = Vec::new();
        let entries = tracing::debug_span!("materialize_entries")
            .in_scope(|| entries_and_payloads(artifact, &mut payloads));
        self.payloads = payloads;
        self.cached_entries = entries.clone();
        tracing::info!(
            path = %resolved.path.display(),
            entries = entries.len(),
            payloads = self.payloads.len(),
            "code graph mention source reloaded"
        );
        self.cache_key = Some(CodeGraphCacheKey {
            path: resolved.path,
            format: resolved.format,
            mtime: modified,
            len,
            content_hash,
        });
        Ok(entries)
    }

    fn code_payloads(&self) -> &[(String, Arc<CodeMentionPayload>)] {
        &self.payloads
    }
}

fn read_resolved_content_hash(resolved: &ResolvedArtifact) -> anyhow::Result<Option<String>> {
    match resolved.format {
        ArtifactFormat::Parquet => Ok(Some(
            read_artifact_header_parquet(&resolved.path)?.graph_content_hash,
        )),
        ArtifactFormat::LegacyJson => Ok(read_artifact_header(&resolved.path)?.content_hash_blake3),
    }
}

fn entries_and_payloads(
    artifact: GraphIndexArtifact,
    payloads: &mut Vec<(String, Arc<CodeMentionPayload>)>,
) -> Vec<MentionEntry> {
    let graph_index_version = artifact.header.graph_index_version;
    let mut entries = Vec::with_capacity(artifact.files.len() + artifact.symbols.len());

    for file in artifact.files {
        let uri = format!("{}{}", CODE_FILE_URI_PREFIX, file.stable_file_id);
        let display = file.file_path.clone();
        payloads.push((
            uri.clone(),
            Arc::new(file_payload(&file, &uri, &display, &graph_index_version)),
        ));
        entries.push(MentionEntry {
            section_header: None,
            kind: MentionKind::CodeFile,
            uri,
            display,
            secondary: None,
            code_path: Some(file.file_path.clone()),
            code_scope: None,
            tag: Some("file".to_string()),
            search_text: Some(file.file_path),
            atom_text: None,
            issue_preview: None,
        });
    }

    for symbol in artifact.symbols {
        let uri = format!("{}{}", CODE_SYMBOL_URI_PREFIX, symbol.stable_symbol_id);
        let display = symbol.entity_name.clone();
        let secondary = symbol_secondary(&symbol);
        let code_scope = symbol.enclosing_scope.clone();
        let atom_text = code_scope
            .as_ref()
            .filter(|scope| !scope.is_empty())
            .map(|scope| format!("@{}::{}", scope, display));
        payloads.push((
            uri.clone(),
            Arc::new(symbol_payload(
                &symbol,
                &uri,
                &display,
                &graph_index_version,
            )),
        ));
        entries.push(MentionEntry {
            section_header: None,
            kind: MentionKind::CodeSymbol,
            uri,
            display,
            secondary: Some(secondary),
            code_path: Some(symbol.file_path.clone()),
            code_scope,
            tag: Some(format!("symbol:{}", symbol.symbol_kind)),
            search_text: Some(symbol_search_text(&symbol)),
            atom_text,
            issue_preview: None,
        });
    }

    entries
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

fn symbol_secondary(symbol: &GraphSymbolArtifact) -> String {
    if let Some(scope) = &symbol.enclosing_scope {
        format!(
            "{}::{} · {}:{} ({})",
            scope, symbol.entity_name, symbol.file_path, symbol.line_range[0], symbol.symbol_kind
        )
    } else {
        format!(
            "{} · {}:{} ({})",
            symbol.entity_name, symbol.file_path, symbol.line_range[0], symbol.symbol_kind
        )
    }
}

fn symbol_search_text(symbol: &GraphSymbolArtifact) -> String {
    let mut text = format!(
        "{} {} {}",
        symbol.entity_name, symbol.symbol_kind, symbol.file_path
    );
    if let Some(scope) = &symbol.enclosing_scope {
        text.push(' ');
        text.push_str(scope);
    }
    text
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
        write_fixture_with_hash(path, file_path, None);
    }

    fn write_fixture_with_hash(path: &Path, file_path: &str, content_hash_blake3: Option<&str>) {
        let header = match content_hash_blake3 {
            Some(hash) => format!(
                r#""header": {{ "graph_index_version": "v1", "content_hash_blake3": "{hash}" }}"#
            ),
            None => r#""header": { "graph_index_version": "v1" }"#.to_string(),
        };
        let artifact = format!(
            r#"{{
  {header},
  "manifest_version": "v1",
  "file_manifests": [],
  "files": [
    {{ "stable_file_id": "file-1", "file_path": "{file_path}" }}
  ],
  "symbols": []
}}"#
        );
        fs::write(path, artifact).expect("write fixture");
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
        let artifact_path = temp.path().join("graph-index.json");
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
        let previous_len = stable_metadata.len();
        write_fixture(&artifact_path, "src/lib_rewritten_with_longer_name.rs");
        let stable_filetime = FileTime::from_system_time(stable_mtime);
        set_file_mtime(&artifact_path, stable_filetime).expect("restore mtime");
        let current_len = fs::metadata(&artifact_path).expect("metadata").len();
        assert_ne!(current_len, previous_len);

        let fourth = source.build(Path::new(".")).expect("fourth build");
        assert_eq!(fourth.len(), 1);
        assert_eq!(fourth[0].display, "src/lib_rewritten_with_longer_name.rs");
        assert_eq!(source.reload_count, 3);
    }

    #[test]
    fn code_payloads_cache_hit_reuses_arc_instances() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.json");
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
    fn build_uses_content_hash_when_metadata_changes_but_content_does_not() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("graph-index.json");
        write_fixture_with_hash(&artifact_path, "src/main.rs", Some("hash-same"));
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let fixture_bytes = fs::read(&artifact_path).expect("read fixture bytes");
        let previous_mtime = fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes_with(&artifact_path, previous_mtime, || {
            fs::write(&artifact_path, &fixture_bytes).expect("rewrite identical bytes");
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
        let artifact_path = temp.path().join("graph-index.json");
        write_fixture_with_hash(&artifact_path, "src/main.rs", Some("hash-before"));
        let mut source = CodeGraphMentionSource::new(&artifact_path);

        let first = source.build(Path::new(".")).expect("first build");
        assert_eq!(first.len(), 1);
        assert_eq!(source.reload_count, 1);

        let previous_mtime = fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes_with(&artifact_path, previous_mtime, || {
            write_fixture_with_hash(&artifact_path, "src/lib.rs", Some("hash-after"));
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
        }
    }
}
