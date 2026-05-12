use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

pub mod validation;

pub const CODE_FILE_URI_PREFIX: &str = "graph://file/";
pub const CODE_SYMBOL_URI_PREFIX: &str = "graph://symbol/";

pub type SourceRange = [usize; 2];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeMentionKind {
    File,
    Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMentionPayload {
    pub authoritative: CodeMentionAuthoritative,
    pub extraction_hints: CodeMentionExtractionHints,
    pub display_meta: CodeMentionDisplayMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMentionAuthoritative {
    pub display: String,
    pub uri: String,
    pub kind: CodeMentionKind,
    pub file_path: String,
    pub validation: CodeMentionValidationSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeMentionValidationSpec {
    FileExists {
        path: String,
    },
    SymbolRange {
        path: String,
        line_range: SourceRange,
        byte_range: SourceRange,
        entity_name: String,
        anchor_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMentionExtractionHints {
    pub line_range: Option<SourceRange>,
    pub byte_range: Option<SourceRange>,
    pub symbol_kind: Option<String>,
    pub entity_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMentionDisplayMeta {
    pub enclosing_scope: Option<String>,
    pub graph_index_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIndexArtifact {
    pub header: GraphIndexHeader,
    pub files: Vec<GraphFileArtifact>,
    pub symbols: Vec<GraphSymbolArtifact>,
    #[serde(default, skip)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIndexHeader {
    pub graph_index_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphFileArtifact {
    pub stable_file_id: String,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphSymbolArtifact {
    pub stable_symbol_id: String,
    pub file_path: String,
    pub byte_range: SourceRange,
    pub line_range: SourceRange,
    pub entity_name: String,
    pub symbol_kind: String,
    pub anchor_hash: String,
    pub enclosing_scope: Option<String>,
}

pub fn load_artifact(path: &Path) -> anyhow::Result<GraphIndexArtifact> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read graph index artifact `{}`", path.display()))?;
    let mut artifact: GraphIndexArtifact = serde_json::from_str(&content)
        .map_err(|err| anyhow!("invalid graph index JSON in `{}`: {err}", path.display()))?;

    deduplicate_symbols(&mut artifact);
    validate_ranges(&artifact)?;

    Ok(artifact)
}

fn deduplicate_symbols(artifact: &mut GraphIndexArtifact) {
    let mut seen = HashSet::new();
    let mut diagnostics = Vec::new();
    artifact.symbols.retain(|symbol| {
        if seen.insert(symbol.stable_symbol_id.clone()) {
            true
        } else {
            diagnostics.push(format!(
                "duplicate stable_symbol_id `{}` ignored after first occurrence",
                symbol.stable_symbol_id
            ));
            false
        }
    });
    artifact.diagnostics = diagnostics;
}

fn validate_ranges(artifact: &GraphIndexArtifact) -> anyhow::Result<()> {
    for symbol in &artifact.symbols {
        if symbol.byte_range[1] < symbol.byte_range[0] {
            return Err(anyhow!(
                "graph index symbol `{}` has reversed byte_range [{}, {}]",
                symbol.stable_symbol_id,
                symbol.byte_range[0],
                symbol.byte_range[1]
            ));
        }
    }
    Ok(())
}
