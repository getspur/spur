use anyhow::Context;
use serde::Serialize;

use crate::{
    GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
    GraphSymbolArtifact, GraphTombstoneEntry,
};

#[derive(Serialize)]
pub(crate) struct GraphArtifactBodyForHash<'a> {
    pub files: &'a [GraphFileArtifact],
    pub symbols: &'a [GraphSymbolArtifact],
    pub edges: &'a [GraphEdgeArtifact],
    pub file_manifests: &'a [GraphFileManifestEntry],
    pub graph_content_hash: &'a str,
    pub manifest_version: &'a str,
    pub tombstones: &'a [GraphTombstoneEntry],
}

pub fn artifact_content_hash_blake3_hex(artifact: &GraphIndexArtifact) -> anyhow::Result<String> {
    let body = GraphArtifactBodyForHash {
        files: &artifact.files,
        symbols: &artifact.symbols,
        edges: &artifact.edges,
        file_manifests: &artifact.file_manifests,
        graph_content_hash: &artifact.graph_content_hash,
        manifest_version: &artifact.manifest_version,
        tombstones: &artifact.tombstones,
    };
    let canonical_json = serde_json::to_vec(&body)
        .context("failed to encode graph artifact body for content hash")?;
    Ok(blake3::hash(&canonical_json).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Confidence, GraphEdgeKind, GraphIndexHeader, NodeId, RelationKind};

    #[test]
    fn canonical_bytes_snapshot_guard() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "v5".to_string(),
                content_hash_blake3: Some("header-content-hash".to_string()),
            },
            manifest_version: "manifest-v1".to_string(),
            graph_content_hash: "graph-content-hash".to_string(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: "file:src/lib.rs".to_string(),
                path: "src/lib.rs".to_string(),
                content_oid: "blob-oid".to_string(),
                node_ids: vec![NodeId(1), NodeId(2)],
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: "file:src/lib.rs".to_string(),
                file_path: "src/lib.rs".to_string(),
            }],
            file_node_ids: vec![NodeId(1)],
            symbols: vec![GraphSymbolArtifact {
                stable_symbol_id: "sym:src/lib.rs:demo".to_string(),
                file_path: "src/lib.rs".to_string(),
                byte_range: [10, 42],
                line_range: [2, 5],
                entity_name: "demo".to_string(),
                qualified_name: "crate::demo".to_string(),
                symbol_kind: "function".to_string(),
                anchor_hash: "anchor-hash".to_string(),
                enclosing_scope: Some("crate".to_string()),
            }],
            symbol_node_ids: vec![NodeId(2)],
            edges: vec![GraphEdgeArtifact {
                source_stable_symbol_id: "sym:src/lib.rs:demo".to_string(),
                target_stable_symbol_id: Some("sym:src/lib.rs:helper".to_string()),
                target_label: Some("helper".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::SyntaxExact,
                confidence_score: 0.875,
                change_kind: None,
                edge_kind: Some(GraphEdgeKind::Calls),
            }],
            tombstones: vec![GraphTombstoneEntry {
                path: "src/old.rs".to_string(),
                stable_file_id: "file:src/old.rs".to_string(),
            }],
            diagnostics: vec!["not part of canonical hash body".to_string()],
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        };
        let body = GraphArtifactBodyForHash {
            files: &artifact.files,
            symbols: &artifact.symbols,
            edges: &artifact.edges,
            file_manifests: &artifact.file_manifests,
            graph_content_hash: &artifact.graph_content_hash,
            manifest_version: &artifact.manifest_version,
            tombstones: &artifact.tombstones,
        };
        let bytes = serde_json::to_vec(&body).expect("canonical JSON should serialize");

        insta::assert_snapshot!(
            "canonical_bytes_layout",
            std::str::from_utf8(&bytes).expect("canonical JSON is UTF-8")
        );
    }
}
