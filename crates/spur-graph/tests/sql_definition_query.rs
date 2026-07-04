use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SQL_TAGS_QUERY: &str = include_str!("../queries/sql/tags.scm");

fn parse_sql(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
    let tree = parse_sql(source);
    let query = Query::new(&language, SQL_TAGS_QUERY).expect("compile query");
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

fn build_sql_fixture(source: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("schema.sql"), source).expect("write schema.sql");
    build_facts(dir.path(), None).expect("extract").0
}

#[test]
fn sql_tags_query_captures_trigger_and_index_definitions() {
    let source = r#"
CREATE TABLE tbl (c INTEGER);
CREATE TRIGGER t BEFORE INSERT ON tbl FOR EACH ROW EXECUTE FUNCTION audit();
CREATE INDEX idx ON tbl(c);
"#;
    let tree = parse_sql(source);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );

    assert_eq!(definition_names(source, "definition.function"), ["t"]);
    assert_eq!(definition_names(source, "definition.constant"), ["idx"]);
}

#[test]
fn sql_extractor_builds_trigger_and_index_symbols() {
    let source = r#"
CREATE TABLE tbl (c INTEGER);
CREATE TRIGGER t BEFORE INSERT ON tbl FOR EACH ROW EXECUTE FUNCTION audit();
CREATE INDEX idx ON tbl(c);
"#;
    let facts = build_sql_fixture(source);
    let has_node = |kind: NodeKind, label: &str| {
        facts
            .nodes
            .iter()
            .any(|node| node.kind == kind && node.label == label)
    };

    assert!(has_node(NodeKind::Function, "t"));
    assert!(has_node(NodeKind::Constant, "idx"));
}
