use spur_graph::{build_facts, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/cpp/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == capture_name {
            names.push(
                capture
                    .node
                    .utf8_text(source.as_bytes())
                    .expect("text")
                    .to_owned(),
            );
        }
    }

    names
}

#[test]
fn cpp_captures_base_class() {
    let source =
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override {} };\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(names.contains(&"Base".to_owned()), "got {names:?}");
}

#[test]
fn cpp_class_without_base_has_no_extends() {
    let source = "struct Plain { int x; };\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(names.is_empty(), "got {names:?}");
}

#[test]
fn cpp_derived_types_emit_extends_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.cpp"),
        r#"
struct StructBase {};
struct StructDerived : StructBase {};

class ClassBase {};
class ClassDerived : public ClassBase {};

class Plain {};
"#,
    )
    .expect("write fixture");

    let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
    let edge_summary = || {
        facts
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.relation,
                    edge.source_node_id,
                    edge.target_label.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let source_label = |source_node_id| {
        facts
            .nodes
            .iter()
            .find(|node| node.node_id == source_node_id)
            .map(|node| node.label.as_str())
    };
    let has_extends = |source: &str, target: &str| {
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends
                && source_label(edge.source_node_id) == Some(source)
                && edge.target_label.as_deref() == Some(target)
        })
    };

    assert!(
        has_extends("StructDerived", "StructBase"),
        "expected StructDerived extends StructBase; got {:?}",
        edge_summary()
    );
    assert!(
        has_extends("ClassDerived", "ClassBase"),
        "expected ClassDerived extends ClassBase; got {:?}",
        edge_summary()
    );

    let plain = facts
        .nodes
        .iter()
        .find(|node| node.label == "Plain")
        .expect("Plain class node");
    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends && edge.source_node_id == plain.node_id
        }),
        "Plain must not emit extends; got {:?}",
        edge_summary()
    );
}
