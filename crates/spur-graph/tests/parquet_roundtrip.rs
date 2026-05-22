use std::path::Path;

use spur_graph::{
    read_artifact_header_parquet, read_artifact_parquet, write_artifact_parquet, Confidence,
    GraphArtifactManifest, GraphEdgeArtifact, GraphEdgeKind, GraphFileArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphSymbolArtifact,
    GraphTombstoneEntry, NodeId, RelationKind, WriteOptions,
};

#[test]
fn parquet_artifact_round_trips_all_tables_with_exact_node_ids() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = fixture_artifact();

    assert!(!WriteOptions::default().emit_edges_by_dst);

    let dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions {
            emit_edges_by_dst: true,
        },
    )
    .expect("write parquet artifact");

    assert_parquet_files_exist(&dir);

    let manifest = read_artifact_header_parquet(&dir).expect("read manifest");
    assert!(manifest.complete);
    assert!(manifest.edges_by_dst_present);
    assert_eq!(manifest.row_counts.nodes, 2);
    assert_eq!(manifest.row_counts.edges, 2);
    assert_eq!(manifest.row_counts.edges_by_dst, Some(2));
    assert_eq!(manifest.row_counts.edges_unresolved, 1);
    assert_eq!(manifest.row_counts.files, 3);
    assert_eq!(manifest.row_counts.file_manifests, 3);
    assert_eq!(manifest.row_counts.tombstones, 1);
    assert_eq!(manifest.parquet_writer.row_group_size, 16_384);
    assert_eq!(manifest.parquet_writer.compression, "zstd-3");

    let actual = read_artifact_parquet(&dir).expect("read parquet artifact");
    assert_artifact_eq(&actual, &artifact);
}

#[test]
fn rejects_directory_without_manifest() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let dir = write_artifact_parquet(&fixture_artifact(), tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    std::fs::remove_file(dir.join("manifest.json")).expect("remove manifest");

    let err = read_artifact_parquet(&dir).expect_err("missing manifest must be rejected");

    assert!(
        err.to_string().contains("manifest.json"),
        "error should mention manifest.json: {err:#}"
    );
}

#[test]
fn rejects_directory_with_incomplete_manifest() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let dir = write_artifact_parquet(&fixture_artifact(), tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let manifest_path = dir.join("manifest.json");
    let mut manifest: GraphArtifactManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest.complete = false;
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
    )
    .expect("write incomplete manifest");

    let err = read_artifact_parquet(&dir).expect_err("incomplete manifest must be rejected");

    assert!(
        err.to_string().contains("complete"),
        "error should mention complete: {err:#}"
    );
}

#[test]
fn write_replaces_existing_hash_directory_before_publish() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = fixture_artifact();
    let dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions {
            emit_edges_by_dst: true,
        },
    )
    .expect("write parquet artifact");
    let stale = dir.join("stale-file");
    std::fs::write(&stale, b"stale").expect("write stale file");

    let rewritten = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("rewrite parquet artifact");

    assert_eq!(rewritten, dir);
    assert!(
        !stale.exists(),
        "existing hash directory should be removed before publication"
    );
    assert!(!dir.join("edges_by_dst.parquet").exists());
}

fn fixture_artifact() -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "spur-graph-phase2".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "test-manifest-version".to_string(),
        graph_content_hash: "test-graph-content-hash".to_string(),
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
            GraphFileManifestEntry {
                stable_file_id: "file-c".to_string(),
                path: "src/c.rs".to_string(),
                content_oid: "cccccccccccccccccccccccccccccccccccccccc".to_string(),
                node_ids: Vec::new(),
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
            GraphFileArtifact {
                stable_file_id: "file-c".to_string(),
                file_path: "src/c.rs".to_string(),
            },
        ],
        file_node_ids: vec![NodeId(10), NodeId(20), NodeId(30)],
        symbols: vec![
            GraphSymbolArtifact {
                stable_symbol_id: "sym-a-fn".to_string(),
                file_path: "src/a.rs".to_string(),
                byte_range: [10, 42],
                line_range: [2, 5],
                entity_name: "a_fn".to_string(),
                qualified_name: "crate::a::a_fn".to_string(),
                symbol_kind: "function".to_string(),
                anchor_hash: "anchor-a".to_string(),
                enclosing_scope: Some("mod a".to_string()),
            },
            GraphSymbolArtifact {
                stable_symbol_id: "sym-b-fn".to_string(),
                file_path: "src/b.rs".to_string(),
                byte_range: [3, 19],
                line_range: [1, 3],
                entity_name: "b_fn".to_string(),
                qualified_name: "crate::b::b_fn".to_string(),
                symbol_kind: "function".to_string(),
                anchor_hash: "anchor-b".to_string(),
                enclosing_scope: None,
            },
        ],
        symbol_node_ids: vec![NodeId(11), NodeId(21)],
        edges: vec![
            GraphEdgeArtifact {
                source_stable_symbol_id: "file-a".to_string(),
                target_stable_symbol_id: Some("sym-a-fn".to_string()),
                target_label: Some("a_fn".to_string()),
                relation: RelationKind::Contains,
                confidence: Confidence::SyntaxExact,
                confidence_score: 1.0,
                edge_kind: Some(GraphEdgeKind::ReferencesOther),
            },
            GraphEdgeArtifact {
                source_stable_symbol_id: "sym-a-fn".to_string(),
                target_stable_symbol_id: Some("sym-b-fn".to_string()),
                target_label: Some("b_fn".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::SyntaxExact,
                confidence_score: 0.875,
                edge_kind: Some(GraphEdgeKind::Calls),
            },
            GraphEdgeArtifact {
                source_stable_symbol_id: "sym-b-fn".to_string(),
                target_stable_symbol_id: None,
                target_label: Some("missing_fn".to_string()),
                relation: RelationKind::Calls,
                confidence: Confidence::Heuristic,
                confidence_score: f32::from_bits(0x7fc0_1234),
                edge_kind: Some(GraphEdgeKind::CallsDyn),
            },
        ],
        tombstones: vec![GraphTombstoneEntry {
            path: "src/removed.rs".to_string(),
            stable_file_id: "file-removed".to_string(),
        }],
        diagnostics: Vec::new(),
    }
}

fn assert_parquet_files_exist(dir: &Path) {
    for name in [
        "nodes.parquet",
        "edges.parquet",
        "edges_by_dst.parquet",
        "edges_unresolved.parquet",
        "files.parquet",
        "file_manifests.parquet",
        "tombstones.parquet",
        "manifest.json",
    ] {
        assert!(dir.join(name).exists(), "{name} should exist");
    }
}

fn assert_artifact_eq(actual: &GraphIndexArtifact, expected: &GraphIndexArtifact) {
    assert_eq!(actual.header, expected.header);
    assert_eq!(actual.manifest_version, expected.manifest_version);
    assert_eq!(actual.graph_content_hash, expected.graph_content_hash);
    assert_eq!(actual.file_manifests, expected.file_manifests);
    assert_eq!(actual.files, expected.files);
    assert_eq!(actual.file_node_ids, expected.file_node_ids);
    assert_eq!(actual.symbols, expected.symbols);
    assert_eq!(actual.symbol_node_ids, expected.symbol_node_ids);
    assert_eq!(actual.tombstones, expected.tombstones);
    assert_eq!(actual.diagnostics, expected.diagnostics);
    assert_eq!(actual.edges.len(), expected.edges.len());
    for (actual, expected) in actual.edges.iter().zip(&expected.edges) {
        assert_eq!(
            actual.source_stable_symbol_id,
            expected.source_stable_symbol_id
        );
        assert_eq!(
            actual.target_stable_symbol_id,
            expected.target_stable_symbol_id
        );
        assert_eq!(actual.target_label, expected.target_label);
        assert_eq!(actual.relation, expected.relation);
        assert_eq!(actual.confidence, expected.confidence);
        assert_eq!(
            actual.confidence_score.to_bits(),
            expected.confidence_score.to_bits()
        );
        assert_eq!(actual.edge_kind, expected.edge_kind);
    }
}
