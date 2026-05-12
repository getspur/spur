use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use spur_graph::extract::tree_sitter::extract_rust_worktree;
use spur_graph::graph::petgraph_builder::build_petgraph;
use spur_graph::load_artifact;
use spur_graph::store::json::{artifact_from_facts, write_artifact};
use spur_graph::{Confidence, NodeKind, RelationKind};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_corpus")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_corpus/expected_graph_index.json")
}

fn nested_fn_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nested_fn_corpus")
}

#[test]
fn rust_extractor_matches_sample_corpus_golden_artifact() {
    let root = fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let actual = serde_json::to_string_pretty(&artifact).expect("encode artifact");
    let actual = format!("{actual}\n");

    if std::env::var_os("SPUR_GRAPH_BLESS").is_some() {
        fs::write(golden_path(), &actual).expect("write golden artifact");
    }

    let expected = fs::read_to_string(golden_path()).expect("read golden artifact");
    assert_eq!(actual, expected);
}

#[test]
fn rust_extractor_keeps_nested_functions_inside_methods_as_functions() {
    let root = nested_fn_fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");

    let baz = facts
        .nodes
        .iter()
        .find(|node| node.label == "baz")
        .expect("nested function is extracted");

    assert_eq!(baz.kind, NodeKind::Function);
}

#[test]
fn rust_extractor_finds_expected_nodes_edges_and_spans() {
    let root = fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");

    let labels: BTreeSet<_> = facts.nodes.iter().map(|node| node.label.as_str()).collect();
    assert!(labels.contains("src/lib.rs"));
    assert!(labels.contains("src/utils.rs"));
    assert!(labels.contains("inline"));
    assert!(labels.contains("App"));
    assert!(labels.contains("Runner"));
    assert!(labels.contains("Mode"));
    assert!(labels.contains("run"));
    assert!(labels.contains("build_app"));
    assert!(labels.contains("helper"));
    assert!(labels.contains("label"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Impl && node.label == "App"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Impl && node.label == "Helper"));

    assert_eq!(
        facts
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::File))
            .count(),
        2
    );
    assert!(
        facts.nodes.len() >= 12,
        "expected fixture symbols plus files, got {}",
        facts.nodes.len()
    );
    assert!(
        facts.edges.len() >= 10,
        "expected contains/import/call edges, got {}",
        facts.edges.len()
    );

    let contains_edges = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Contains)
        .count();
    let imports_edges = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Imports)
        .count();
    let calls_edges = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Calls)
        .count();

    assert!(contains_edges >= 8, "contains edges: {contains_edges}");
    assert!(imports_edges >= 2, "imports edges: {imports_edges}");
    assert!(calls_edges >= 1, "calls edges: {calls_edges}");

    for span in &facts.spans {
        assert!(
            span.end_byte > span.start_byte,
            "invalid byte span: {span:?}"
        );
        assert!(
            span.end_line >= span.start_line,
            "invalid line span: {span:?}"
        );
    }
}

#[test]
fn rust_extractor_stable_keys_are_deterministic_across_runs() {
    let root = fixture_root();
    let first = extract_rust_worktree(&root).expect("first extract");
    let second = extract_rust_worktree(&root).expect("second extract");

    let first_keys: Vec<_> = first
        .nodes
        .iter()
        .map(|node| (node.kind, node.label.clone(), node.stable_key.clone()))
        .collect();
    let second_keys: Vec<_> = second
        .nodes
        .iter()
        .map(|node| (node.kind, node.label.clone(), node.stable_key.clone()))
        .collect();

    assert_eq!(first_keys, second_keys);
}

#[test]
fn rust_extractor_tags_edge_confidence_by_relation_semantics() {
    let root = fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");

    let contains_edge = facts
        .edges
        .iter()
        .find(|edge| edge.relation == RelationKind::Contains)
        .expect("fixture has contains edge");
    let imports_edge = facts
        .edges
        .iter()
        .find(|edge| edge.relation == RelationKind::Imports)
        .expect("fixture has imports edge");
    let calls_edge = facts
        .edges
        .iter()
        .find(|edge| edge.relation == RelationKind::Calls)
        .expect("fixture has calls edge");

    assert_eq!(contains_edge.confidence, Confidence::SyntaxExact);
    assert_eq!(contains_edge.confidence_score, 1.0);
    assert_eq!(imports_edge.confidence, Confidence::Heuristic);
    assert_eq!(imports_edge.confidence_score, 0.8);
    assert_eq!(calls_edge.confidence, Confidence::Heuristic);
    assert_eq!(calls_edge.confidence_score, 0.8);
}

#[test]
fn petgraph_builder_preserves_typed_fact_counts() {
    let root = fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");
    let graph = build_petgraph(&facts).expect("build petgraph");

    assert_eq!(graph.node_count(), facts.nodes.len());
    assert_eq!(graph.edge_count(), facts.edges.len());
}

#[test]
fn artifact_writer_round_trips_through_existing_reader() {
    let root = fixture_root();
    let facts = extract_rust_worktree(&root).expect("extract fixture");
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph_index.json");

    write_artifact(&artifact, &path).expect("write artifact");
    let loaded = load_artifact(&path).expect("existing reader loads writer output");

    assert_eq!(loaded.files, artifact.files);
    assert_eq!(loaded.symbols, artifact.symbols);
    assert!(loaded.symbols.iter().any(|symbol| {
        symbol.entity_name == "run"
            && symbol.symbol_kind == "method"
            && symbol.enclosing_scope.as_deref() == Some("impl App")
    }));
}
