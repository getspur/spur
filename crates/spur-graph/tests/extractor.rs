use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use spur_graph::build_facts;
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

fn python_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_corpus")
}

fn python_golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/python_corpus/expected_graph_index.json")
}

fn python_nested_fn_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_nested_fn_corpus")
}

fn python_decorated_method_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_decorated_method_corpus")
}

fn typescript_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typescript_corpus")
}

fn typescript_golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/typescript_corpus/expected_graph_index.json")
}

fn markdown_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown_corpus")
}

fn markdown_golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/markdown_corpus/expected_graph_index.json")
}

#[test]
fn rust_extractor_matches_sample_corpus_golden_artifact() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
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
fn python_extractor_matches_sample_corpus_golden_artifact() {
    let root = python_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let actual = serde_json::to_string_pretty(&artifact).expect("encode artifact");
    let actual = format!("{actual}\n");

    if std::env::var_os("SPUR_GRAPH_BLESS").is_some() {
        fs::write(python_golden_path(), &actual).expect("write golden artifact");
    }

    let expected = fs::read_to_string(python_golden_path()).expect("read golden artifact");
    assert_eq!(actual, expected);
}

#[test]
fn typescript_extractor_matches_typescript_corpus_golden_artifact() {
    let root = typescript_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let actual = serde_json::to_string_pretty(&artifact).expect("encode artifact");
    let actual = format!("{actual}\n");

    if std::env::var_os("SPUR_GRAPH_BLESS").is_some() {
        fs::write(typescript_golden_path(), &actual).expect("write golden artifact");
    }

    let expected = fs::read_to_string(typescript_golden_path()).expect("read golden artifact");
    assert_eq!(actual, expected);
}

#[test]
fn markdown_extractor_matches_corpus_golden_artifact() {
    let root = markdown_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let actual = serde_json::to_string_pretty(&artifact).expect("encode artifact");
    let actual = format!("{actual}\n");

    if std::env::var_os("SPUR_GRAPH_BLESS").is_some() {
        fs::write(markdown_golden_path(), &actual).expect("write golden artifact");
    }

    let expected = fs::read_to_string(markdown_golden_path()).expect("read golden artifact");
    assert_eq!(actual, expected);
}

#[test]
fn markdown_extractor_builds_section_hierarchy_and_link_edges() {
    let root = markdown_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;

    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Section && node.label == "Overview"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Section && node.label == "Details"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Section && node.label == "Appendix"));

    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Contains));
    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Links));
}

#[test]
fn rust_extractor_keeps_nested_functions_inside_methods_as_functions() {
    let root = nested_fn_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;

    let baz = facts
        .nodes
        .iter()
        .find(|node| node.label == "baz")
        .expect("nested function is extracted");

    assert_eq!(baz.kind, NodeKind::Function);
}

#[test]
fn python_extractor_keeps_nested_functions_inside_methods_as_functions() {
    let root = python_nested_fn_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;

    let outer = facts
        .nodes
        .iter()
        .find(|node| node.label == "outer")
        .expect("method is extracted");
    let inner = facts
        .nodes
        .iter()
        .find(|node| node.label == "inner")
        .expect("nested function is extracted");

    assert_eq!(outer.kind, NodeKind::Method);
    assert_eq!(inner.kind, NodeKind::Function);
}

#[test]
fn python_extractor_classifies_decorated_methods_as_methods() {
    let root = python_decorated_method_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");

    for method_name in ["name", "helper", "from_str"] {
        let method_node = facts
            .nodes
            .iter()
            .find(|node| node.label == method_name)
            .unwrap_or_else(|| panic!("expected method node: {method_name}"));
        let method_symbol = artifact
            .symbols
            .iter()
            .find(|symbol| symbol.entity_name == method_name)
            .unwrap_or_else(|| panic!("expected symbol: {method_name}"));

        assert_eq!(
            method_node.kind,
            NodeKind::Method,
            "node kind for {method_name}"
        );
        assert_eq!(
            method_symbol.enclosing_scope.as_deref(),
            Some("Foo"),
            "enclosing scope for {method_name}"
        );
    }
}

#[test]
fn rust_extractor_finds_expected_nodes_edges_and_spans() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;

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
fn typescript_extractor_finds_expected_nodes_and_edges() {
    let root = typescript_fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let node_labels_by_id: std::collections::HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node.label.as_str()))
        .collect();

    let labels: BTreeSet<_> = facts.nodes.iter().map(|node| node.label.as_str()).collect();
    assert!(labels.contains("src/helpers.ts"));
    assert!(labels.contains("src/app.tsx"));
    assert!(labels.contains("Helper"));
    assert!(labels.contains("App"));
    assert!(labels.contains("Runner"));
    assert!(labels.contains("Mode"));
    assert!(labels.contains("renderThing"));
    assert!(labels.contains("createApp"));
    assert!(labels.contains("Result"));
    assert!(labels.contains("AppResult"));
    assert!(labels.contains("boot"));
    assert!(labels.contains("run"));
    assert!(labels.contains("Greeting"));
    assert!(labels.contains("helper"));

    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Class && node.label == "Helper"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Class && node.label == "App"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Interface && node.label == "Runner"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::TypeAlias && node.label == "Result"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::TypeAlias && node.label == "AppResult"));

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

    assert_eq!(imports_edges, 3, "imports edges: {imports_edges}");
    assert!(calls_edges >= 2, "calls edges: {calls_edges}");

    let app_file_id = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.label == "src/app.tsx")
        .expect("app.tsx file node")
        .node_id;
    let app_import_targets: BTreeSet<_> = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Imports && edge.source_node_id == app_file_id)
        .map(|edge| {
            *node_labels_by_id
                .get(&edge.target_node_id)
                .expect("import target node exists")
        })
        .collect();
    assert_eq!(
        app_import_targets,
        BTreeSet::from(["Helper", "Mode", "renderThing"])
    );
}

#[test]
fn rust_extractor_stable_keys_are_deterministic_across_runs() {
    let root = fixture_root();
    let first = build_facts(&root).expect("first extract").0;
    let second = build_facts(&root).expect("second extract").0;

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
    let facts = build_facts(&root).expect("extract fixture").0;

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
    let facts = build_facts(&root).expect("extract fixture").0;
    let graph = build_petgraph(&facts).expect("build petgraph");

    assert_eq!(graph.node_count(), facts.nodes.len());
    assert_eq!(graph.edge_count(), facts.edges.len());
}

#[test]
fn artifact_writer_round_trips_through_existing_reader() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
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
