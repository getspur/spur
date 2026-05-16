use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::mentions::entry::{MentionEntry, MentionKind, MentionSource};
use spur_graph::{
    load_artifact, CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints,
    CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec, GraphFileArtifact,
    GraphIndexArtifact, GraphSymbolArtifact, CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX,
};

pub struct CodeGraphMentionSource {
    artifact_path: PathBuf,
    cache_key: Option<(PathBuf, SystemTime)>,
    cached_entries: Vec<MentionEntry>,
    payloads: Vec<(String, Arc<CodeMentionPayload>)>,
    #[cfg(test)]
    reload_count: usize,
}

impl CodeGraphMentionSource {
    pub fn new(artifact_path: impl Into<PathBuf>) -> Self {
        Self {
            artifact_path: artifact_path.into(),
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

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let modified = fs::metadata(&self.artifact_path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let cached = matches!(
            (&self.cache_key, modified),
            (Some((path, cached_mtime)), Some(current_mtime))
                if path == &self.artifact_path && *cached_mtime == current_mtime
        );
        let span = tracing::info_span!(
            "code_graph_build",
            path = %self.artifact_path.display(),
            cached = cached
        );
        let _guard = span.enter();

        if cached {
            return Ok(self.cached_entries.clone());
        }

        if !self.artifact_path.is_file() {
            self.cache_key = None;
            self.cached_entries.clear();
            self.payloads.clear();
            return Ok(Vec::new());
        }

        #[cfg(test)]
        {
            self.reload_count += 1;
        }

        let artifact = match tracing::debug_span!("load_artifact")
            .in_scope(|| load_artifact(&self.artifact_path))
        {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %self.artifact_path.display(),
                    "code graph mention source disabled for unreadable artifact"
                );
                self.cache_key = None;
                self.cached_entries.clear();
                self.payloads.clear();
                return Ok(Vec::new());
            }
        };
        for diagnostic in &artifact.diagnostics {
            tracing::warn!(
                diagnostic = diagnostic.as_str(),
                path = %self.artifact_path.display(),
                "code graph mention source loaded artifact with diagnostic"
            );
        }

        let mut payloads = Vec::new();
        let entries = tracing::debug_span!("materialize_entries")
            .in_scope(|| entries_and_payloads(artifact, &mut payloads));
        self.payloads = payloads;
        self.cached_entries = entries.clone();
        self.cache_key = modified.map(|mtime| (self.artifact_path.clone(), mtime));
        Ok(entries)
    }

    fn code_payloads(&self) -> &[(String, Arc<CodeMentionPayload>)] {
        &self.payloads
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
            tag: Some(format!("symbol:{}", symbol.symbol_kind)),
            search_text: Some(symbol_search_text(&symbol)),
            atom_text: None,
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
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: symbol.enclosing_scope.clone(),
            graph_index_version: graph_index_version.to_string(),
        },
    }
}

fn symbol_secondary(symbol: &GraphSymbolArtifact) -> String {
    let mut secondary = format!(
        "{} {}:{}-{}",
        symbol.symbol_kind, symbol.file_path, symbol.line_range[0], symbol.line_range[1]
    );
    if let Some(scope) = &symbol.enclosing_scope {
        secondary.push(' ');
        secondary.push_str(scope);
    }
    secondary
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
    use std::time::Duration;

    fn write_fixture(path: &Path, file_path: &str) {
        let artifact = format!(
            r#"{{
  "header": {{ "graph_index_version": "v1" }},
  "manifest_version": "v1",
  "file_manifests": [],
  "files": [
    {{ "stable_file_id": "file-1", "file_path": "{file_path}" }}
  ],
  "symbols": []
}}"#
        );
        std::fs::write(path, artifact).expect("write fixture");
    }

    fn rewrite_until_mtime_changes(path: &Path, file_path: &str, previous: SystemTime) {
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            write_fixture(path, file_path);
            let current = std::fs::metadata(path)
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

        let previous_mtime = std::fs::metadata(&artifact_path)
            .expect("metadata")
            .modified()
            .expect("modified");
        rewrite_until_mtime_changes(&artifact_path, "src/lib.rs", previous_mtime);

        let third = source.build(Path::new(".")).expect("third build");
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].display, "src/lib.rs");
        assert_eq!(source.reload_count, 2);
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
}
