use std::path::{Path, PathBuf};

use crate::mentions::entry::{MentionEntry, MentionKind, MentionSource};
use spur_graph::{
    load_artifact, CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints,
    CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec, GraphFileArtifact,
    GraphIndexArtifact, GraphSymbolArtifact, CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX,
};

pub struct CodeGraphMentionSource {
    artifact_path: PathBuf,
    payloads: Vec<(String, CodeMentionPayload)>,
}

impl CodeGraphMentionSource {
    pub fn new(artifact_path: impl Into<PathBuf>) -> Self {
        Self {
            artifact_path: artifact_path.into(),
            payloads: Vec::new(),
        }
    }
}

impl MentionSource for CodeGraphMentionSource {
    fn name(&self) -> &'static str {
        "code_graph"
    }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        self.payloads.clear();

        if !self.artifact_path.is_file() {
            return Ok(Vec::new());
        }

        let artifact = match load_artifact(&self.artifact_path) {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %self.artifact_path.display(),
                    "code graph mention source disabled for unreadable artifact"
                );
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

        Ok(entries_and_payloads(artifact, &mut self.payloads))
    }

    fn code_payloads(&self) -> Vec<(String, CodeMentionPayload)> {
        self.payloads.clone()
    }
}

fn entries_and_payloads(
    artifact: GraphIndexArtifact,
    payloads: &mut Vec<(String, CodeMentionPayload)>,
) -> Vec<MentionEntry> {
    let graph_index_version = artifact.header.graph_index_version;
    let mut entries = Vec::with_capacity(artifact.files.len() + artifact.symbols.len());

    for file in artifact.files {
        let uri = format!("{}{}", CODE_FILE_URI_PREFIX, file.stable_file_id);
        let display = file.file_path.clone();
        payloads.push((
            uri.clone(),
            file_payload(&file, &uri, &display, &graph_index_version),
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
            symbol_payload(&symbol, &uri, &display, &graph_index_version),
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
