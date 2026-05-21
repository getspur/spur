use std::collections::HashSet;
use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::{EdgeId, EvidenceId, FileId, NodeId, RunId, SpanId};

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub qualified_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMentionDisplayMeta {
    pub enclosing_scope: Option<String>,
    pub graph_index_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIndexArtifact {
    pub header: GraphIndexHeader,
    #[serde(default)]
    pub manifest_version: String,
    #[serde(default)]
    pub graph_content_hash: String,
    #[serde(default)]
    pub file_manifests: Vec<GraphFileManifestEntry>,
    pub files: Vec<GraphFileArtifact>,
    #[serde(skip)]
    pub file_node_ids: Vec<NodeId>,
    pub symbols: Vec<GraphSymbolArtifact>,
    #[serde(skip)]
    pub symbol_node_ids: Vec<NodeId>,
    #[serde(default)]
    pub edges: Vec<GraphEdgeArtifact>,
    #[serde(default)]
    pub tombstones: Vec<GraphTombstoneEntry>,
    #[serde(default, skip)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIndexHeader {
    pub graph_index_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash_blake3: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphFileArtifact {
    pub stable_file_id: String,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphFileManifestEntry {
    pub stable_file_id: String,
    pub path: String,
    pub content_oid: String,
    pub node_ids: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphTombstoneEntry {
    pub path: String,
    pub stable_file_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIndexPointer {
    pub schema: String,
    pub graph_content_hash: String,
    pub manifest_version: String,
    pub source_kind: SourceKind,
    pub indexed_commit_oid: Option<String>,
    pub canonical_artifact_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Git,
    Fs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphSymbolArtifact {
    pub stable_symbol_id: String,
    pub file_path: String,
    pub byte_range: SourceRange,
    pub line_range: SourceRange,
    pub entity_name: String,
    #[serde(default)]
    pub qualified_name: String,
    pub symbol_kind: String,
    pub anchor_hash: String,
    pub enclosing_scope: Option<String>,
}

// Symbol artifacts already persist byte/line ranges, so edge artifacts only carry
// stable symbol identifiers and relation metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdgeArtifact {
    pub source_stable_symbol_id: String,
    pub target_stable_symbol_id: Option<String>,
    pub target_label: Option<String>,
    pub relation: RelationKind,
    pub confidence: Confidence,
    pub confidence_score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_kind: Option<GraphEdgeKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub node_id: NodeId,
    pub stable_key: String,
    pub label: String,
    pub kind: NodeKind,
    pub file_id: Option<FileId>,
    pub source_span_id: Option<SpanId>,
    pub first_seen_run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    pub edge_id: EdgeId,
    pub source_node_id: NodeId,
    pub target_node_id: Option<NodeId>,
    pub relation: RelationKind,
    pub target_label: Option<String>,
    pub confidence: Confidence,
    pub confidence_score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_kind: Option<GraphEdgeKind>,
    pub evidence_id: EvidenceId,
    pub directed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub span_id: SpanId,
    pub file_id: FileId,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Module,
    Function,
    Class,
    Interface,
    Struct,
    Impl,
    Trait,
    Enum,
    File,
    Method,
    Field,
    Constant,
    TypeAlias,
    Macro,
    Section,
    McpTool,
}

impl NodeKind {
    pub fn discriminator(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Function => "function",
            NodeKind::Class => "class",
            NodeKind::Interface => "interface",
            NodeKind::Method => "method",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            NodeKind::Trait => "trait",
            NodeKind::Impl => "impl",
            NodeKind::Field => "field",
            NodeKind::Constant => "constant",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Macro => "macro",
            NodeKind::Section => "section",
            NodeKind::McpTool => "mcp_tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Imports,
    Calls,
    Contains,
    Implements,
    Defines,
    References,
    Uses,
    Extends,
    Links,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    Calls,
    CallsDyn,
    ReferencesHof,
    ReferencesOther,
}

pub fn graph_edge_kind_or_default(
    relation: RelationKind,
    edge_kind: Option<GraphEdgeKind>,
) -> GraphEdgeKind {
    edge_kind.unwrap_or(match relation {
        RelationKind::Calls => GraphEdgeKind::Calls,
        RelationKind::References => GraphEdgeKind::ReferencesOther,
        _ => GraphEdgeKind::ReferencesOther,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    SyntaxExact,
    Heuristic,
    Unknown,
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

/// Reads only the top-level graph index header from an artifact file.
///
/// This intentionally avoids allocating large artifact arrays such as `files`
/// and `symbols` in memory, but still parses the full JSON stream (O(file_size)
/// I/O/CPU) to reach the header field.
pub fn read_artifact_header(path: &Path) -> anyhow::Result<GraphIndexHeader> {
    #[derive(Deserialize)]
    struct ArtifactHeaderEnvelope {
        header: GraphIndexHeader,
    }

    let file = fs::File::open(path)
        .with_context(|| format!("failed to read graph index artifact `{}`", path.display()))?;
    let reader = BufReader::new(file);
    let envelope: ArtifactHeaderEnvelope = serde_json::from_reader(reader)
        .map_err(|err| anyhow!("invalid graph index JSON in `{}`: {err}", path.display()))?;

    Ok(envelope.header)
}

pub fn file_id_from_uri(uri: &str) -> String {
    uri.strip_prefix(CODE_FILE_URI_PREFIX)
        .unwrap_or(uri)
        .to_string()
}

pub fn symbol_id_from_uri(uri: &str) -> String {
    uri.strip_prefix(CODE_SYMBOL_URI_PREFIX)
        .unwrap_or(uri)
        .to_string()
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
