use spur_graph::{build_facts, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/python/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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
fn py_captures_base_class() {
    let source = "class Base:\n    pass\nclass Derived(Base):\n    pass\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(names.contains(&"Base".to_owned()), "got {names:?}");
}

#[test]
fn py_captures_multiple_bases() {
    let source = "class A:\n    pass\nclass B:\n    pass\nclass C(A, B):\n    pass\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(
        names.contains(&"A".to_owned()) && names.contains(&"B".to_owned()),
        "got {names:?}"
    );
}

#[test]
fn py_plain_class_and_keyword_args_have_no_extends() {
    let plain = capture_texts(SPUR_EDGES_QUERY, "class Plain:\n    pass\n", "extends.name");
    assert!(plain.is_empty(), "got {plain:?}");

    let keyword = capture_texts(
        SPUR_EDGES_QUERY,
        "class M(metaclass=Meta):\n    pass\n",
        "extends.name",
    );
    assert!(
        !keyword.contains(&"Meta".to_owned()),
        "metaclass kwarg captured as base: {keyword:?}"
    );
}

#[test]
fn py_classes_emit_extends_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.py"),
        r#"
class Base:
    pass

class Derived(Base):
    pass

class A:
    pass

class B:
    pass

class C(A, B):
    pass

class Plain:
    pass

class M(metaclass=Meta):
    pass
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
    let class_node = |label: &str| {
        facts
            .nodes
            .iter()
            .find(|node| node.label == label)
            .unwrap_or_else(|| panic!("{label} class node"))
    };

    assert!(
        has_extends("Derived", "Base"),
        "expected Derived extends Base; got {:?}",
        edge_summary()
    );
    assert!(
        has_extends("C", "A"),
        "expected C extends A; got {:?}",
        edge_summary()
    );
    assert!(
        has_extends("C", "B"),
        "expected C extends B; got {:?}",
        edge_summary()
    );

    let plain = class_node("Plain");
    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends && edge.source_node_id == plain.node_id
        }),
        "Plain must not emit extends; got {:?}",
        edge_summary()
    );

    let metaclass = class_node("M");
    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends
                && edge.source_node_id == metaclass.node_id
                && edge.target_label.as_deref() == Some("Meta")
        }),
        "metaclass keyword must not emit extends; got {:?}",
        edge_summary()
    );
}
