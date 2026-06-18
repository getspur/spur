use std::collections::BTreeSet;

use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/python/spur-edges.scm");

fn capture_texts(source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, SPUR_EDGES_QUERY).expect("compile query");
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
                    .expect("capture text")
                    .to_owned(),
            );
        }
    }

    names
}

fn capture_set(source: &str, capture_name: &str) -> BTreeSet<String> {
    capture_texts(source, capture_name).into_iter().collect()
}

fn edge_target_label<'a>(
    facts: &'a spur_graph::extract::GraphFacts,
    edge: &'a spur_graph::GraphEdge,
) -> Option<&'a str> {
    edge.target_node_id
        .and_then(|node_id| {
            facts
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .map(|node| node.label.as_str())
        })
        .or(edge.target_label.as_deref())
}

#[test]
fn python_spur_edges_query_captures_import_and_import_from_variants() {
    let source = r#"
import os
import pkg.util as util
from pkg.api import Client
from pkg.api import helper as aliased_helper
from .local import Local
"#;

    let import_names = capture_set(source, "import.name");
    for target in ["os", "util", "Client", "helper", "Local"] {
        assert!(
            import_names.contains(target),
            "missing Python import capture {target}; imports: {import_names:?}"
        );
    }

    let import_paths = capture_set(source, "import.path");
    for target in ["os", "pkg.util", "pkg.api", ".local"] {
        assert!(
            import_paths.contains(target),
            "missing Python import_path capture {target}; paths: {import_paths:?}"
        );
    }
}

#[test]
fn python_graph_resolves_import_from_edges_by_module_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("pkg")).expect("mkdir pkg");
    std::fs::write(root.join("pkg/__init__.py"), "").expect("write __init__.py");
    std::fs::write(
        root.join("pkg/api.py"),
        r#"
class Client:
    pass

def helper():
    pass
"#,
    )
    .expect("write api.py");
    std::fs::write(
        root.join("main.py"),
        r#"
from pkg.api import Client
from pkg.api import helper

def run():
    helper()
"#,
    )
    .expect("write main.py");

    let (facts, _counts) = build_facts(root, None).expect("build facts");
    let main_file = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.label == "main.py")
        .expect("main.py file node");
    let has_import = |target: &str| {
        facts.edges.iter().any(|edge| {
            edge.source_node_id == main_file.node_id
                && edge.relation == RelationKind::Imports
                && edge.import_path.as_deref() == Some("pkg.api")
                && edge_target_label(&facts, edge) == Some(target)
        })
    };

    for target in ["Client", "helper"] {
        assert!(
            has_import(target),
            "missing resolved Python import edge to {target}; edges: {:?}",
            facts
                .edges
                .iter()
                .map(|edge| (
                    edge.relation,
                    edge.import_path.clone(),
                    edge_target_label(&facts, edge)
                ))
                .collect::<Vec<_>>()
        );
    }
}
