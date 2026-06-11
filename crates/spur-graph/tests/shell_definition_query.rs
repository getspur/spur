use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SHELL_TAGS_QUERY: &str = include_str!("../queries/shell/tags.scm");

fn parse_shell(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    let tree = parse_shell(source);
    let query = Query::new(&language, SHELL_TAGS_QUERY).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();

    while let Some(query_match) = matches.next() {
        let has_definition = query_match
            .captures
            .iter()
            .any(|capture| capture_names[capture.index as usize] == definition_capture_name);

        if has_definition {
            names.extend(query_match.captures.iter().filter_map(|capture| {
                if capture_names[capture.index as usize] == "name" {
                    Some(
                        capture
                            .node
                            .utf8_text(source.as_bytes())
                            .expect("capture text")
                            .to_owned(),
                    )
                } else {
                    None
                }
            }));
        }
    }

    names
}

#[test]
fn shell_tags_query_captures_function_definitions() {
    let source = r#"
#!/usr/bin/env bash

prepare() {
  echo "prepare"
}

function deploy {
  prepare
}
"#;
    let tree = parse_shell(source);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );

    assert_eq!(
        definition_names(source, "definition.function"),
        ["prepare", "deploy"]
    );
}

#[test]
fn shell_extractor_builds_symbols_imports_and_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("scripts")).expect("mkdir scripts");
    fs::write(
        root.join("scripts/deploy.sh"),
        r#"
#!/usr/bin/env bash
source ./lib.sh

prepare() {
  echo "prepare"
}

deploy() {
  prepare
  ./run-task.sh
}
"#,
    )
    .expect("write deploy.sh");

    let facts = build_facts(root, None).expect("extract").0;

    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Function && node.label == "prepare"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Function && node.label == "deploy"));
    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Imports
            && edge.target_label.as_deref() == Some("./lib.sh")));
    assert!(facts.edges.iter().any(|edge| {
        edge.relation == RelationKind::Calls && edge.target_label.as_deref() == Some("prepare")
    }));
}
