use spur_graph::{
    graph_edge_kind_or_default, load_artifact, Confidence, EdgeId, EvidenceId, FileId, GraphEdge,
    GraphEdgeArtifact, GraphEdgeKind, GraphNode, GraphSymbolArtifact, NodeId, NodeKind,
    RelationKind, RunId, SourceSpan, SpanId, SymbolSnapshotArtifact,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

#[test]
fn graph_facts_round_trip_through_json() {
    let node = GraphNode {
        node_id: NodeId(7),
        stable_key: "rust:src/lib.rs:run".to_string(),
        label: "run".to_string(),
        kind: NodeKind::Function,
        file_id: Some(FileId(3)),
        source_span_id: Some(SpanId(11)),
        first_seen_run_id: RunId(19),
    };
    let edge = GraphEdge {
        edge_id: EdgeId(5),
        source_node_id: NodeId(7),
        target_node_id: Some(NodeId(8)),
        relation: RelationKind::Calls,
        target_label: Some("callee".to_string()),
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: None,
        evidence_id: EvidenceId(13),
        directed: true,
    };
    let span = SourceSpan {
        span_id: SpanId(11),
        file_id: FileId(3),
        start_byte: 10,
        end_byte: 42,
        start_line: 2,
        end_line: 4,
    };

    let encoded = serde_json::to_string(&(node.clone(), edge.clone(), span.clone())).unwrap();
    let decoded: (GraphNode, GraphEdge, SourceSpan) = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, (node, edge, span));
}

#[test]
fn syntax_exact_confidence_round_trips_as_snake_case_json() {
    let encoded = serde_json::to_string(&Confidence::SyntaxExact).unwrap();

    assert_eq!(encoded, "\"syntax_exact\"");
    assert_eq!(
        serde_json::from_str::<Confidence>(&encoded).unwrap(),
        Confidence::SyntaxExact
    );
}

#[test]
fn graph_edge_kind_round_trips_all_public_values_and_legacy_omission() {
    for edge_kind in [
        GraphEdgeKind::Calls,
        GraphEdgeKind::CallsDyn,
        GraphEdgeKind::ReferencesHof,
        GraphEdgeKind::ReferencesOther,
    ] {
        let edge = GraphEdgeArtifact {
            source_stable_symbol_id: "source".to_string(),
            target_stable_symbol_id: Some("target".to_string()),
            target_label: Some("target".to_string()),
            relation: RelationKind::Calls,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            change_kind: None,

            edge_kind: Some(edge_kind),
        };
        let encoded = serde_json::to_string(&edge).unwrap();
        assert!(
            encoded.contains("\"edge_kind\""),
            "serialized edge must persist edge_kind: {encoded}"
        );
        let decoded: GraphEdgeArtifact = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.edge_kind, Some(edge_kind));
    }

    let legacy = r#"{
        "source_stable_symbol_id":"source",
        "target_stable_symbol_id":"target",
        "target_label":"target",
        "relation":"calls",
        "confidence":"syntax_exact",
        "confidence_score":1.0
    }"#;
    let decoded: GraphEdgeArtifact = serde_json::from_str(legacy).unwrap();
    assert_eq!(decoded.edge_kind, None);
}

#[test]
fn legacy_artifact_references_edges_without_edge_kind_count_as_references_other() {
    let dir = TempDir::new().unwrap();
    let artifact_path = dir.path().join("graph-index.json");
    std::fs::write(
        &artifact_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "header": {
                "graph_index_version": "v1"
            },
            "manifest_version": "test",
            "graph_content_hash": "test",
            "files": [
                { "stable_file_id": "file-src-lib", "file_path": "src/lib.rs" }
            ],
            "symbols": [
                {
                    "stable_symbol_id": "source",
                    "file_path": "src/lib.rs",
                    "byte_range": [0, 8],
                    "line_range": [1, 1],
                    "entity_name": "source",
                    "qualified_name": "source",
                    "symbol_kind": "function",
                    "anchor_hash": "hash-source",
                    "enclosing_scope": null
                },
                {
                    "stable_symbol_id": "target",
                    "file_path": "src/lib.rs",
                    "byte_range": [10, 18],
                    "line_range": [3, 3],
                    "entity_name": "target",
                    "qualified_name": "target",
                    "symbol_kind": "function",
                    "anchor_hash": "hash-target",
                    "enclosing_scope": null
                }
            ],
            "edges": [
                {
                    "source_stable_symbol_id": "source",
                    "target_stable_symbol_id": "target",
                    "target_label": "target",
                    "relation": "references",
                    "confidence": "syntax_exact",
                    "confidence_score": 1.0
                }
            ],
            "tombstones": []
        }))
        .unwrap(),
    )
    .unwrap();

    let artifact = load_artifact(&artifact_path).unwrap();
    assert_eq!(artifact.edges[0].edge_kind, None);

    let mut references_hof = 0usize;
    let mut references_other = 0usize;
    for edge in &artifact.edges {
        match graph_edge_kind_or_default(edge.relation, edge.edge_kind) {
            GraphEdgeKind::ReferencesHof => references_hof += 1,
            GraphEdgeKind::ReferencesOther => references_other += 1,
            GraphEdgeKind::Calls | GraphEdgeKind::CallsDyn => {}
        }
    }

    assert_eq!(references_hof, 0);
    assert_eq!(references_other, 1);
}

#[test]
fn graph_id_newtypes_are_not_interchangeable_at_runtime() {
    let node_id = NodeId(42);
    let edge_id = EdgeId(42);
    let json = serde_json::to_string(&node_id).unwrap();

    assert_eq!(json, "42");
    assert_eq!(node_id.get(), 42);
    assert_eq!(edge_id.get(), 42);
    assert_ne!(format!("{node_id:?}"), format!("{edge_id:?}"));
}

#[test]
fn node_kind_discriminators_are_stable_contracts() {
    assert_eq!(NodeKind::File.discriminator(), "file");
    assert_eq!(NodeKind::Module.discriminator(), "module");
    assert_eq!(NodeKind::Function.discriminator(), "function");
    assert_eq!(NodeKind::Class.discriminator(), "class");
    assert_eq!(NodeKind::Interface.discriminator(), "interface");
    assert_eq!(NodeKind::Method.discriminator(), "method");
    assert_eq!(NodeKind::Struct.discriminator(), "struct");
    assert_eq!(NodeKind::Enum.discriminator(), "enum");
    assert_eq!(NodeKind::Trait.discriminator(), "trait");
    assert_eq!(NodeKind::Impl.discriminator(), "impl");
    assert_eq!(NodeKind::Field.discriminator(), "field");
    assert_eq!(NodeKind::Constant.discriminator(), "constant");
    assert_eq!(NodeKind::TypeAlias.discriminator(), "type_alias");
    assert_eq!(NodeKind::Macro.discriminator(), "macro");
    assert_eq!(NodeKind::Commit.discriminator(), "commit");
}

#[test]
fn structural_symbol_ids_match_temporal_snapshot_ids_for_fixture_corpora() {
    for fixture_root in [sample_corpus_root(), nested_fn_corpus_root()] {
        let repo = committed_fixture_repo(&fixture_root);
        let facts = spur_graph::build_facts(repo.path())
            .expect("extract structural facts")
            .0;
        let structural =
            spur_graph::store::build::artifact_from_facts(&facts, repo.path()).expect("artifact");
        let (temporal, _) =
            spur_graph::git_walk::run_full_walk_into(repo.path(), &Default::default())
                .expect("temporal walk");

        for symbol in &structural.symbols {
            let snapshot =
                matching_snapshot(symbol, &temporal.symbol_snapshots).unwrap_or_else(|| {
                    let candidates: Vec<_> = temporal
                        .symbol_snapshots
                        .iter()
                        .filter(|snapshot| {
                            snapshot.file_path.to_path_buf() == Path::new(&symbol.file_path)
                                && snapshot.entity_name == symbol.entity_name
                                && snapshot.symbol_kind == symbol.symbol_kind
                        })
                        .collect();
                    panic!("missing temporal snapshot for {symbol:?}; candidates: {candidates:#?}")
                });
            assert_eq!(
                symbol.stable_symbol_id, snapshot.key.stable_symbol_id,
                "stable id mismatch for {} {} in {} at {:?}",
                symbol.symbol_kind, symbol.qualified_name, symbol.file_path, symbol.byte_range
            );
        }
    }
}

fn sample_corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_corpus")
}

fn nested_fn_corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nested_fn_corpus")
}

fn committed_fixture_repo(fixture_root: &Path) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    copy_dir(fixture_root, dir.path());
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(
        dir.path(),
        &["config", "user.email", "schema-test@example.com"],
    );
    git(dir.path(), &["config", "user.name", "Schema Test"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "fixture"]);
    dir
}

fn copy_dir(source: &Path, destination: &Path) {
    for entry in std::fs::read_dir(source).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            std::fs::create_dir_all(&destination_path).expect("create fixture subdir");
            copy_dir(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).expect("copy fixture file");
        }
    }
}

fn git(worktree: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(worktree)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn matching_snapshot<'a>(
    symbol: &GraphSymbolArtifact,
    snapshots: &'a [SymbolSnapshotArtifact],
) -> Option<&'a SymbolSnapshotArtifact> {
    snapshots.iter().find(|snapshot| {
        snapshot.file_path.to_path_buf() == Path::new(&symbol.file_path)
            && snapshot.entity_name == symbol.entity_name
            && snapshot.symbol_kind == symbol.symbol_kind
            && snapshot.byte_range == symbol.byte_range
            && snapshot.line_range == symbol.line_range
    })
}
