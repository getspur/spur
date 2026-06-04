use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/typescript/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
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
fn ts_captures_implemented_interface() {
    let source = "interface I { f(): void }\nclass C implements I { f() {} }\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");

    assert!(names.contains(&"I".to_owned()), "got {names:?}");
}

#[test]
fn ts_captures_extended_base_class() {
    let source = "class B {}\nclass C extends B {}\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(names.contains(&"B".to_owned()), "got {names:?}");
}

#[test]
fn ts_plain_class_has_no_heritage_edges() {
    let source = "class Plain {}\n";

    let implements_names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");
    let extends_names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");

    assert!(
        implements_names.is_empty(),
        "plain class must not emit implements; got {implements_names:?}"
    );
    assert!(
        extends_names.is_empty(),
        "plain class must not emit extends; got {extends_names:?}"
    );
}

#[test]
fn ts_class_emits_implements_and_extends_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.ts"),
        "interface I { f(): void }\nclass B {}\nclass C extends B implements I { f() {} }\n",
    )
    .expect("write fixture");

    let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
    let class_c = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.label == "C")
        .expect("class C node");
    let has = |relation, target| {
        facts.edges.iter().any(|edge| {
            edge.source_node_id == class_c.node_id
                && edge.relation == relation
                && edge.target_label.as_deref() == Some(target)
        })
    };

    assert!(
        has(RelationKind::Implements, "I"),
        "missing C implements I edge; got {:?}",
        facts
            .edges
            .iter()
            .map(|edge| (edge.relation, edge.target_label.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        has(RelationKind::Extends, "B"),
        "missing C extends B edge; got {:?}",
        facts
            .edges
            .iter()
            .map(|edge| (edge.relation, edge.target_label.clone()))
            .collect::<Vec<_>>()
    );
}
