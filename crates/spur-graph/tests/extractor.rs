use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use spur_graph::build_facts;
use spur_graph::graph::petgraph_builder::build_petgraph;
use spur_graph::load_artifact;
use spur_graph::store::json::{
    artifact_from_facts, artifact_from_facts_incremental, write_artifact, BuildMode,
};
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

fn normalize_for_golden(
    mut artifact: spur_graph::GraphIndexArtifact,
) -> spur_graph::GraphIndexArtifact {
    artifact.manifest_version = "<normalized>".to_string();
    for entry in &mut artifact.file_manifests {
        entry.mtime_nanos = 0;
        entry.size_bytes = 0;
    }
    artifact
}

#[test]
fn rust_extractor_matches_sample_corpus_golden_artifact() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = normalize_for_golden(artifact_from_facts(&facts, &root).expect("artifact"));
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
    let artifact = normalize_for_golden(artifact_from_facts(&facts, &root).expect("artifact"));
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
    let artifact = normalize_for_golden(artifact_from_facts(&facts, &root).expect("artifact"));
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
    let artifact = normalize_for_golden(artifact_from_facts(&facts, &root).expect("artifact"));
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
fn stable_key_is_stable_under_leading_whitespace_insertion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");

    let base_source = r#"
trait Foo { fn f(&self); }
struct Bar;
impl Foo for Bar { fn f(&self) {} }
impl Bar { fn a(&self) {} }
impl Bar { fn b(&self) {} }
"#;
    fs::write(root.join("src/lib.rs"), base_source).expect("write base source");
    let base_facts = build_facts(root).expect("extract base").0;
    let base_keys: Vec<_> = base_facts
        .nodes
        .iter()
        .map(|node| (node.kind, node.label.clone(), node.stable_key.clone()))
        .collect();

    fs::write(root.join("src/lib.rs"), format!("\n{base_source}")).expect("write shifted source");
    let shifted_facts = build_facts(root).expect("extract shifted").0;
    let shifted_keys: Vec<_> = shifted_facts
        .nodes
        .iter()
        .map(|node| (node.kind, node.label.clone(), node.stable_key.clone()))
        .collect();

    assert_eq!(
        base_keys, shifted_keys,
        "leading whitespace insertion should not perturb stable keys"
    );
}

#[test]
fn rust_extractor_distinguishes_trait_impls_of_same_self_type() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        r#"
trait Foo { fn f(&self); }
trait Baz { fn b(&self); }
struct Bar;
impl Foo for Bar { fn f(&self) {} }
impl Baz for Bar { fn b(&self) {} }
"#,
    )
    .expect("write lib.rs");

    let facts = build_facts(root).expect("extract").0;
    let impl_nodes: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Impl)
        .collect();

    assert!(
        impl_nodes.iter().any(|node| node.label == "Foo for Bar"),
        "expected trait impl label `Foo for Bar`"
    );
    assert!(
        impl_nodes.iter().any(|node| node.label == "Baz for Bar"),
        "expected trait impl label `Baz for Bar`"
    );

    let keys: BTreeSet<_> = impl_nodes
        .iter()
        .map(|node| node.stable_key.clone())
        .collect();
    assert_eq!(keys.len(), 2, "trait impls must have distinct stable keys");
}

#[test]
fn rust_extractor_distinguishes_multiple_inherent_impls_in_one_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        r#"
struct Bar;
impl Bar { fn a(&self) {} }
impl Bar { fn b(&self) {} }
"#,
    )
    .expect("write lib.rs");

    let facts = build_facts(root).expect("extract").0;
    let impl_nodes: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Impl && node.label == "Bar")
        .collect();
    assert_eq!(impl_nodes.len(), 2, "expected two inherent impl nodes");

    let keys: BTreeSet<_> = impl_nodes
        .iter()
        .map(|node| node.stable_key.clone())
        .collect();
    assert_eq!(
        keys.len(),
        2,
        "inherent impls for same type in one file must have distinct stable keys"
    );
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
fn build_skips_files_with_invalid_utf8_and_continues() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/good.rs"),
        b"pub fn good_function() {}\n" as &[u8],
    )
    .expect("write good.rs");
    // Stray 0xFE byte — invalid UTF-8. Mimics the production failure
    // (docs/research/DEEP_RESEARCH_AGENTFS.md) where a single bad byte
    // used to abort the whole build.
    fs::write(
        root.join("src/bad.rs"),
        b"pub fn other() {\n    let s = \"\xFE\";\n}\n" as &[u8],
    )
    .expect("write bad.rs");

    let (facts, file_counts) = build_facts(root).expect("build must not abort on bad UTF-8");

    assert!(
        facts.nodes.iter().any(|node| node.label == "good_function"),
        "good_function should be extracted from valid file"
    );
    assert!(
        !facts.nodes.iter().any(|node| node.label == "other"),
        "bad.rs should be skipped, not parsed"
    );
    assert_eq!(
        file_counts.get("rust").copied(),
        Some(2),
        "discovery counts both files even though one is skipped during read"
    );
}

#[cfg(unix)]
#[test]
fn discover_files_skips_unreadable_entries_and_continues() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/good.rs"), b"pub fn ok() {}\n" as &[u8]).expect("write good.rs");
    symlink("/nonexistent/target.rs", root.join("src/broken_link.rs"))
        .expect("create broken symlink");

    let (facts, _) = build_facts(root).expect("build must continue after walker errors");

    assert!(
        facts.nodes.iter().any(|node| node.label == "ok"),
        "ok should be extracted from readable file"
    );
}

#[test]
fn artifact_uses_sentinel_anchor_when_source_drifts_after_extraction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/lib.rs"), "pub fn run() {}\n").expect("write lib.rs");

    let facts = build_facts(root).expect("build facts").0;

    fs::write(root.join("src/lib.rs"), "pub fn").expect("truncate lib.rs");

    let artifact = artifact_from_facts(&facts, root).expect("artifact should still serialize");
    let run_symbol = artifact
        .symbols
        .iter()
        .find(|symbol| symbol.entity_name == "run")
        .expect("run symbol should still exist");

    assert_eq!(run_symbol.anchor_hash, "0");
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
    assert_eq!(loaded.edges, artifact.edges);
    assert!(loaded.symbols.iter().any(|symbol| {
        symbol.entity_name == "run"
            && symbol.symbol_kind == "method"
            && symbol.enclosing_scope.as_deref() == Some("impl App")
    }));
}

#[test]
fn artifact_persists_in_file_contains_edges() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");

    let file = artifact
        .files
        .iter()
        .find(|file| file.file_path == "src/lib.rs")
        .expect("lib file artifact");
    let function = artifact
        .symbols
        .iter()
        .find(|symbol| symbol.file_path == "src/lib.rs" && symbol.entity_name == "build_app")
        .expect("build_app symbol");

    assert!(artifact.edges.iter().any(|edge| {
        edge.relation == RelationKind::Contains
            && edge.source_stable_symbol_id == file.stable_file_id
            && edge.target_stable_symbol_id.as_deref() == Some(function.stable_symbol_id.as_str())
            && edge.target_label.is_none()
    }));
}

#[test]
fn artifact_persists_cross_file_calls_edge_with_label() {
    let root = fixture_root();
    let facts = build_facts(&root).expect("extract fixture").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");

    let helper_symbol = artifact
        .symbols
        .iter()
        .find(|symbol| symbol.file_path == "src/utils.rs" && symbol.entity_name == "helper")
        .expect("helper symbol");

    let calls_edge = artifact
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::Calls
                && edge.target_stable_symbol_id.as_deref()
                    == Some(helper_symbol.stable_symbol_id.as_str())
                && edge.target_label.as_deref() == Some("helper")
        })
        .expect("calls edge with retained label");

    assert_eq!(calls_edge.target_label.as_deref(), Some("helper"));
}

#[test]
fn resolve_pending_edges_surfaces_ambiguous_labels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn process() {}\n\
         pub mod inner {\n\
             pub fn process() {}\n\
         }\n",
    )
    .expect("write lib.rs");
    fs::write(
        root.join("src/caller.rs"),
        "use crate::process;\n\
         pub fn call() { process(); }\n",
    )
    .expect("write caller.rs");

    let facts = build_facts(root).expect("extract fixture").0;

    let process_nodes = facts
        .nodes
        .iter()
        .filter(|node| node.label == "process")
        .count();
    assert_eq!(process_nodes, 2, "fixture must contain ambiguous label");

    let labels_by_id: std::collections::HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node.label.as_str()))
        .collect();
    let process_calls = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Calls)
        .filter(|edge| labels_by_id.get(&edge.target_node_id) == Some(&"process"))
        .count();
    assert!(
        process_calls <= 1,
        "calls to ambiguous `process` should resolve to at most one target"
    );
}

#[test]
fn incremental_round_trip_noop_matches_full_artifact() {
    let root = fixture_root();
    let full = artifact_from_facts(&build_facts(&root).expect("extract").0, &root).expect("full");
    let (next, mode) = artifact_from_facts_incremental(&full, &root).expect("incremental");
    assert_eq!(mode, BuildMode::Incremental);
    assert_eq!(next, full);
}

#[test]
fn incremental_round_trip_preserves_edges() {
    let root = fixture_root();
    let full = artifact_from_facts(&build_facts(&root).expect("extract").0, &root).expect("full");
    let (next, mode) = artifact_from_facts_incremental(&full, &root).expect("incremental");
    assert_eq!(mode, BuildMode::Incremental);
    assert_eq!(next.edges, full.edges);
}

#[test]
fn incremental_modify_one_file_replaces_only_that_bucket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
    fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").expect("write b.rs");

    let full = artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("full");
    let before_a = full
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/a.rs")
        .cloned()
        .collect::<Vec<_>>();
    let before_b = full
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/b.rs")
        .cloned()
        .collect::<Vec<_>>();

    sleep(Duration::from_millis(5));
    fs::write(root.join("src/a.rs"), "pub fn alpha2() {}\n").expect("rewrite a.rs");

    let (next, mode) = artifact_from_facts_incremental(&full, root).expect("incremental");
    assert_eq!(mode, BuildMode::Incremental);
    let after_a = next
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/a.rs")
        .cloned()
        .collect::<Vec<_>>();
    let after_b = next
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/b.rs")
        .cloned()
        .collect::<Vec<_>>();
    assert_ne!(after_a, before_a);
    assert_eq!(after_b, before_b);
}

#[test]
fn incremental_delete_file_drops_bucket_and_preserves_others() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
    fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").expect("write b.rs");

    let full = artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("full");
    let before_b = full
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/b.rs")
        .cloned()
        .collect::<Vec<_>>();

    fs::remove_file(root.join("src/a.rs")).expect("delete a.rs");
    let (next, mode) = artifact_from_facts_incremental(&full, root).expect("incremental");
    assert_eq!(mode, BuildMode::Incremental);
    assert!(!next.files.iter().any(|f| f.file_path == "src/a.rs"));
    assert!(!next.symbols.iter().any(|s| s.file_path == "src/a.rs"));
    let after_b = next
        .symbols
        .iter()
        .filter(|s| s.file_path == "src/b.rs")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(after_b, before_b);
}

#[test]
fn incremental_manifest_mismatch_falls_back_to_full() {
    let root = fixture_root();
    let mut full =
        artifact_from_facts(&build_facts(&root).expect("extract").0, &root).expect("full");
    full.manifest_version = "stale-manifest".to_string();

    let (next, mode) = artifact_from_facts_incremental(&full, &root).expect("incremental");
    assert_eq!(mode, BuildMode::Full);
    assert_ne!(next.manifest_version, "stale-manifest");
}
