use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::{EdgeId, EvidenceId, FileId, NodeId, RunId, SpanId};

pub const CODE_FILE_URI_PREFIX: &str = "graph://file/";
pub const CODE_SYMBOL_URI_PREFIX: &str = "graph://symbol/";
pub const GRAPH_INDEX_VERSION_TEMPORAL: &str = "2";

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
    pub symbols: Vec<GraphSymbolArtifact>,
    #[serde(default)]
    pub edges: Vec<GraphEdgeArtifact>,
    #[serde(default)]
    pub tombstones: Vec<GraphTombstoneEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<CommitArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_snapshots: Vec<SymbolSnapshotArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporal_edges: Vec<TemporalEdgeArtifact>,
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
    pub change_kind: Option<ChangeKind>,
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
    pub evidence_id: EvidenceId,
    pub directed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<ChangeKind>,
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
    Commit,
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
            NodeKind::Commit => "commit",
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
    Touches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    SyntaxExact,
    Heuristic,
    Unknown,
}

pub type StableSymbolId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    RenamedFrom(RenamePrev),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenamePrev {
    File(PathBuf),
    Symbol(SnapshotKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotKey {
    pub stable_symbol_id: StableSymbolId,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolSnapshotArtifact {
    pub key: SnapshotKey,
    pub file_path: PathBuf,
    pub entity_name: String,
    pub symbol_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_scope: Option<String>,
    pub byte_range: SourceRange,
    pub line_range: SourceRange,
    pub anchor_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitArtifact {
    pub sha: String,
    pub parents: Vec<String>,
    pub author_time: i64,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WalkStrategy {
    #[default]
    Reachable,
    FirstParent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitIndexArtifact {
    pub schema_version: u32,
    pub commits: Vec<CommitArtifact>,
    pub refs: BTreeMap<String, String>,
    pub indexed_at: String,
    #[serde(default)]
    pub walk_strategy: WalkStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "endpoint", rename_all = "snake_case")]
pub enum EdgeEndpoint {
    File { path: PathBuf },
    Symbol { stable_symbol_id: StableSymbolId },
    Snapshot { key: SnapshotKey },
    Commit { sha: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalEdgeArtifact {
    pub source: EdgeEndpoint,
    pub target: EdgeEndpoint,
    pub relation: RelationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<ChangeKind>,
}

pub fn load_artifact(path: &Path) -> anyhow::Result<GraphIndexArtifact> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read graph index artifact `{}`", path.display()))?;
    let mut artifact: GraphIndexArtifact = serde_json::from_str(&content)
        .map_err(|err| anyhow!("invalid graph index JSON in `{}`: {err}", path.display()))?;
    validate_graph_index_version(&artifact.header.graph_index_version)?;

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

fn validate_graph_index_version(version: &str) -> anyhow::Result<()> {
    if is_supported_graph_index_version(version) {
        return Ok(());
    }

    Err(anyhow!("unsupported graph_index_version `{version}`"))
}

fn is_supported_graph_index_version(version: &str) -> bool {
    matches!(
        version,
        GRAPH_INDEX_VERSION_TEMPORAL | "1" | "v1" | "spur-graph-phase2" | "fixture-2026-05-11"
    )
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
    let mut duplicate_diagnostics = Vec::new();
    artifact.symbols.retain(|symbol| {
        if seen.insert(symbol.stable_symbol_id.clone()) {
            true
        } else {
            duplicate_diagnostics.push(format!(
                "duplicate stable_symbol_id `{}` ignored after first occurrence",
                symbol.stable_symbol_id
            ));
            false
        }
    });
    artifact.diagnostics.extend(duplicate_diagnostics);
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

#[cfg(test)]
mod change_kind_tests {
    use super::*;

    #[test]
    fn change_kind_round_trips_json() {
        let added = ChangeKind::Added;
        let s = serde_json::to_string(&added).unwrap();
        assert_eq!(s, "\"added\"");
        let back: ChangeKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ChangeKind::Added);

        let renamed = ChangeKind::RenamedFrom(RenamePrev::File("src/old.rs".into()));
        let s = serde_json::to_string(&renamed).unwrap();
        let back: ChangeKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, renamed);
    }

    #[test]
    fn node_kind_has_commit_variant() {
        let k = NodeKind::Commit;
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(s, "\"commit\"");
    }

    #[test]
    fn relation_kind_has_touches_variant() {
        let r = RelationKind::Touches;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "\"touches\"");
    }

    #[test]
    fn graph_edge_artifact_change_kind_is_optional() {
        let json = r#"{
            "source_stable_symbol_id":"a",
            "target_stable_symbol_id":"b",
            "target_label":null,
            "relation":"calls",
            "confidence":"syntax_exact",
            "confidence_score":1.0
        }"#;
        let e: GraphEdgeArtifact = serde_json::from_str(json).unwrap();
        assert!(e.change_kind.is_none());
    }
}

#[cfg(test)]
mod temporal_artifact_tests {
    use super::*;

    #[test]
    fn symbol_snapshot_round_trips() {
        let s = SymbolSnapshotArtifact {
            key: SnapshotKey {
                stable_symbol_id: "graph://symbol/foo".to_string(),
                commit: "abc123".to_string(),
            },
            file_path: "src/lib.rs".into(),
            entity_name: "foo".to_string(),
            symbol_kind: "function".to_string(),
            enclosing_scope: None,
            byte_range: [0, 42],
            line_range: [1, 5],
            anchor_hash: "deadbeef".to_string(),
            tokens: vec!["foo".to_string()],
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: SymbolSnapshotArtifact = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn edge_endpoint_serializes_tagged() {
        let e = EdgeEndpoint::Snapshot {
            key: SnapshotKey {
                stable_symbol_id: "x".into(),
                commit: "y".into(),
            },
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"endpoint\":\"snapshot\""));
        let back: EdgeEndpoint = serde_json::from_str(&j).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn graph_index_artifact_temporal_fields_default_empty() {
        let json = r#"{
            "header":{"graph_index_version":"1"},
            "files":[],
            "symbols":[]
        }"#;
        let a: GraphIndexArtifact = serde_json::from_str(json).unwrap();
        assert!(a.commits.is_empty());
        assert!(a.symbol_snapshots.is_empty());
        assert!(a.temporal_edges.is_empty());
    }

    #[test]
    fn temporal_graph_index_version_is_v2() {
        assert_eq!(GRAPH_INDEX_VERSION_TEMPORAL, "2");
    }

    #[test]
    fn load_artifact_rejects_unknown_graph_index_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.json");
        std::fs::write(
            &path,
            r#"{
                "header":{"graph_index_version":"future"},
                "files":[],
                "symbols":[]
            }"#,
        )
        .unwrap();

        let error = load_artifact(&path).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported graph_index_version `future`"));
    }
}
