use spur_graph::{
    load_artifact, write_artifact, CommitArtifact, EdgeEndpoint, GraphFileArtifact,
    GraphIndexArtifact, GraphIndexHeader, RelationKind, SnapshotKey, SymbolSnapshotArtifact,
    TemporalEdgeArtifact, GRAPH_INDEX_VERSION_TEMPORAL,
};

#[test]
fn diagnostics_round_trip_through_json() {
    let mut artifact = minimal_artifact();
    artifact.diagnostics = vec![
        "parse_failed path=src/lib.rs sha=abc123".to_string(),
        "ambiguous_rename stable_symbol_id=sym-main".to_string(),
    ];

    let encoded = serde_json::to_string(&artifact).expect("encode artifact");
    let decoded: GraphIndexArtifact = serde_json::from_str(&encoded).expect("decode artifact");

    assert_eq!(decoded.diagnostics, artifact.diagnostics);
}

#[test]
fn diagnostics_round_trip_through_artifact_io() {
    let mut artifact = minimal_artifact();
    artifact.diagnostics = vec!["parse_failed path=src/lib.rs sha=abc123".to_string()];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph_index.json");

    write_artifact(&artifact, &path).expect("write artifact");
    let decoded = load_artifact(&path).expect("load artifact");

    assert_eq!(decoded.diagnostics, artifact.diagnostics);
}

#[test]
fn artifact_hash_changes_when_temporal_collections_change() {
    let artifact = temporal_artifact();
    let mut mutated = artifact.clone();
    mutated
        .symbol_snapshots
        .push(symbol_snapshot("sym-helper", "c2"));
    let dir = tempfile::tempdir().expect("tempdir");

    let original_hash = write_and_read_content_hash(&artifact, &dir.path().join("original.json"));
    let mutated_hash = write_and_read_content_hash(&mutated, &dir.path().join("mutated.json"));

    assert_ne!(
        original_hash, mutated_hash,
        "changing temporal symbol snapshots should change BLAKE3 content hash"
    );
}

fn minimal_artifact() -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "manifest-v1".to_string(),
        graph_content_hash: "graph-hash-v1".to_string(),
        file_manifests: Vec::new(),
        files: vec![GraphFileArtifact {
            stable_file_id: "file-src-lib".to_string(),
            file_path: "src/lib.rs".to_string(),
        }],
        symbols: Vec::new(),
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn temporal_artifact() -> GraphIndexArtifact {
    let mut artifact = minimal_artifact();
    let snapshot = symbol_snapshot("sym-main", "c1");
    artifact.commits.push(CommitArtifact {
        sha: "c1".to_string(),
        parents: Vec::new(),
        author_time: 1,
        summary: "initial import".to_string(),
    });
    artifact.symbol_snapshots.push(snapshot.clone());
    artifact.temporal_edges.push(TemporalEdgeArtifact {
        source: EdgeEndpoint::Commit {
            sha: "c1".to_string(),
        },
        target: EdgeEndpoint::Snapshot { key: snapshot.key },
        relation: RelationKind::Touches,
        parent: None,
        change_kind: None,
    });
    artifact
}

fn symbol_snapshot(stable_symbol_id: &str, commit: &str) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: stable_symbol_id.to_string(),
            commit: commit.to_string(),
        },
        file_path: "src/lib.rs".into(),
        entity_name: stable_symbol_id.to_string(),
        symbol_kind: "function".to_string(),
        enclosing_scope: None,
        byte_range: [0, 10],
        line_range: [1, 1],
        anchor_hash: format!("anchor-{stable_symbol_id}"),
        tokens: vec![stable_symbol_id.to_string()],
    }
}

fn write_and_read_content_hash(artifact: &GraphIndexArtifact, path: &std::path::Path) -> String {
    write_artifact(artifact, path).expect("write artifact");
    spur_graph::read_artifact_header(path)
        .expect("read artifact header")
        .content_hash_blake3
        .expect("writer should stamp BLAKE3 content hash")
}
