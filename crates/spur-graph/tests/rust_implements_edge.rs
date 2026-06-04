use spur_graph::{build_facts, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/rust/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
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
fn rust_spur_edges_query_captures_implemented_trait_name() {
    let source = "struct Button;\ntrait Drawable { fn draw(&self); }\nimpl Drawable for Button { fn draw(&self) {} }\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");

    assert!(names.contains(&"Drawable".to_owned()), "got {names:?}");
}

#[test]
fn rust_inherent_impl_does_not_capture_implements() {
    let source = "struct Button;\nimpl Button { fn new() -> Self { Button } }\n";

    let names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");

    assert!(
        names.is_empty(),
        "inherent impl must not emit implements; got {names:?}"
    );
}

#[test]
fn rust_impl_emits_implements_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "trait Drawable { fn draw(&self); }\nstruct Button;\nimpl Drawable for Button { fn draw(&self) {} }\n",
    )
    .expect("write fixture");

    let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");

    assert!(
        facts.edges.iter().any(|e| {
            e.relation == RelationKind::Implements && e.target_label.as_deref() == Some("Drawable")
        }),
        "expected an implements edge targeting Drawable; got {:?}",
        facts
            .edges
            .iter()
            .map(|e| (e.relation, e.target_label.clone()))
            .collect::<Vec<_>>()
    );
}
