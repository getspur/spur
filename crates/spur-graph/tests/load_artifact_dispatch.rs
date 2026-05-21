use spur_graph::{
    load_artifact, write_artifact_parquet, Confidence, GraphEdgeArtifact, GraphEdgeKind,
    GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader,
    GraphSymbolArtifact, NodeId, RelationKind, WriteOptions,
};

const GRAPH_CONTENT_HASH: &str = "dispatch-test-graph-content-hash";

#[test]
fn load_artifact_dispatches_directory_to_parquet_reader() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let expected = fixture_artifact();
    let parquet_dir = write_artifact_parquet(&expected, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");

    let loaded = load_artifact(&parquet_dir).expect("load parquet artifact through dispatcher");

    assert_eq!(loaded.graph_content_hash, GRAPH_CONTENT_HASH);
    assert_eq!(loaded, expected);
}

#[test]
fn load_artifact_dispatches_file_to_legacy_json_reader() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let expected = fixture_artifact();
    let json_path = tempdir.path().join("legacy-graph-index.json");
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(&expected).expect("encode legacy artifact"),
    )
    .expect("write legacy artifact");

    let loaded = load_artifact(&json_path).expect("load legacy JSON artifact through dispatcher");

    assert_eq!(loaded.graph_content_hash, GRAPH_CONTENT_HASH);
    assert_common_artifact_fields_eq(&loaded, &expected);
    assert!(loaded.file_node_ids.is_empty());
    assert!(loaded.symbol_node_ids.is_empty());
}

fn assert_common_artifact_fields_eq(actual: &GraphIndexArtifact, expected: &GraphIndexArtifact) {
    assert_eq!(actual.header, expected.header);
    assert_eq!(actual.manifest_version, expected.manifest_version);
    assert_eq!(actual.graph_content_hash, expected.graph_content_hash);
    assert_eq!(actual.file_manifests, expected.file_manifests);
    assert_eq!(actual.files, expected.files);
    assert_eq!(actual.symbols, expected.symbols);
    assert_eq!(actual.edges, expected.edges);
    assert_eq!(actual.tombstones, expected.tombstones);
    assert_eq!(actual.diagnostics, expected.diagnostics);
}

fn fixture_artifact() -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "spur-graph-phase2".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "dispatch-test-manifest-version".to_string(),
        graph_content_hash: GRAPH_CONTENT_HASH.to_string(),
        file_manifests: vec![
            GraphFileManifestEntry {
                stable_file_id: "file-a".to_string(),
                path: "src/a.rs".to_string(),
                content_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                node_ids: vec![NodeId(11)],
            },
            GraphFileManifestEntry {
                stable_file_id: "file-b".to_string(),
                path: "src/b.rs".to_string(),
                content_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                node_ids: vec![NodeId(21)],
            },
        ],
        files: vec![
            GraphFileArtifact {
                stable_file_id: "file-a".to_string(),
                file_path: "src/a.rs".to_string(),
            },
            GraphFileArtifact {
                stable_file_id: "file-b".to_string(),
                file_path: "src/b.rs".to_string(),
            },
        ],
        file_node_ids: vec![NodeId(10), NodeId(20)],
        symbols: vec![
            GraphSymbolArtifact {
                stable_symbol_id: "sym-a".to_string(),
                file_path: "src/a.rs".to_string(),
                byte_range: [1, 12],
                line_range: [1, 2],
                entity_name: "a_fn".to_string(),
                qualified_name: "crate::a::a_fn".to_string(),
                symbol_kind: "function".to_string(),
                anchor_hash: "anchor-a".to_string(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "sym-b".to_string(),
                file_path: "src/b.rs".to_string(),
                byte_range: [3, 18],
                line_range: [2, 4],
                entity_name: "b_fn".to_string(),
                qualified_name: "crate::b::b_fn".to_string(),
                symbol_kind: "function".to_string(),
                anchor_hash: "anchor-b".to_string(),
                enclosing_scope: Some("mod b".to_string()),
            },
        ],
        symbol_node_ids: vec![NodeId(11), NodeId(21)],
        edges: vec![
            GraphEdgeArtifact {
                source_stable_symbol_id: "file-a".to_string(),
                target_stable_symbol_id: Some("sym-a".to_string()),
                target_label: Some("a_fn".to_string()),
                relation: RelationKind::Contains,
                confidence: Confidence::SyntaxExact,
                confidence_score: 1.0,
                edge_kind: Some(GraphEdgeKind::ReferencesOther),
            },
            GraphEdgeArtifact {
                source_stable_symbol_id: "sym-a".to_string(),
                target_stable_symbol_id: Some("sym-b".to_string()),
                target_label: Some("b_fn".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::SyntaxExact,
                confidence_score: 0.875,
                edge_kind: Some(GraphEdgeKind::Calls),
            },
            GraphEdgeArtifact {
                source_stable_symbol_id: "sym-b".to_string(),
                target_stable_symbol_id: None,
                target_label: Some("missing_fn".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::Heuristic,
                confidence_score: 0.5,
                edge_kind: Some(GraphEdgeKind::CallsDyn),
            },
        ],
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
    }
}
