use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const RUST_TAGS_QUERY: &str = include_str!("../queries/rust/tags.scm");
const RUST_SPUR_EDGES_QUERY: &str = include_str!("../queries/rust/spur-edges.scm");

fn definition_names(
    query_source: &str,
    source: &str,
    definition_capture_name: &str,
) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
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
fn rust_spur_edges_query_captures_enum_variant_definitions() {
    let source = r"
enum DatasourceKind {
    Csv,
    Parquet,
    Json,
}
";

    let names = definition_names(RUST_SPUR_EDGES_QUERY, source, "definition.enum_variant");

    assert_eq!(names, ["Csv", "Parquet", "Json"]);
}

#[test]
fn rust_tags_query_captures_canonical_definition_extensions() {
    let source = r"
type Rows = Vec<String>;

macro_rules! make_event {
    () => {};
}

union Payload {
    raw: u32,
}
";

    assert_eq!(
        definition_names(RUST_TAGS_QUERY, source, "definition.type_alias"),
        ["Rows"]
    );
    assert_eq!(
        definition_names(RUST_TAGS_QUERY, source, "definition.macro"),
        ["make_event"]
    );
    assert_eq!(
        definition_names(RUST_TAGS_QUERY, source, "definition.struct"),
        ["Payload"]
    );
}

#[test]
fn rust_extractor_indexes_new_definition_kinds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/lib.rs"),
        r"
enum DatasourceKind {
    Csv,
    Parquet,
}

type Rows = Vec<String>;

macro_rules! make_event {
    () => {};
}

union Payload {
    raw: u32,
}
",
    )
    .expect("write lib.rs");

    let facts = build_facts(root, None).expect("extract").0;

    let enum_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Enum && node.label == "DatasourceKind")
        .expect("DatasourceKind enum symbol");
    let csv_node = facts
        .nodes
        .iter()
        .find(|node| node.kind.discriminator() == "enum_variant" && node.label == "Csv")
        .expect("Csv enum variant symbol");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Contains
                && edge.source_node_id == enum_node.node_id
                && edge.target_node_id == Some(csv_node.node_id)
        }),
        "expected Csv enum variant symbol"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::TypeAlias && node.label == "Rows"),
        "expected Rows type alias symbol"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Macro && node.label == "make_event"),
        "expected make_event macro symbol"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Struct && node.label == "Payload"),
        "expected Payload union symbol as struct kind"
    );
}
