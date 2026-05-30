use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const CPP_TAGS_QUERY: &str = include_str!("../queries/cpp/tags.scm");

fn parse(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn root_sexp(source: &str) -> String {
    parse(source).root_node().to_sexp()
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let tree = parse(source);
    let query = Query::new(&language, CPP_TAGS_QUERY).expect("compile query");
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
fn cpp_tags_query_captures_enumerators() {
    let source = r#"
namespace demo {
enum class Color {
  Red,
  Green = 2,
};
}
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("enumerator_list"), "{sexp}");
    assert!(sexp.contains("enumerator"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.enum_variant"),
        ["Red", "Green"]
    );
}

#[test]
fn cpp_tags_query_captures_namespace_scope_constants_only() {
    let source = r#"
const int FILE_LIMIT = 5;

namespace demo {
constexpr double PI = 3.14;
static constexpr int BUFFER_SIZE = 10;
const int answer();

int scale(const int factor) {
  const int local_limit = factor * BUFFER_SIZE;
  return local_limit;
}
}
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("translation_unit"), "{sexp}");
    assert!(sexp.contains("namespace_definition"), "{sexp}");
    assert!(sexp.contains("declaration"), "{sexp}");
    assert!(sexp.contains("init_declarator"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.constant"),
        ["FILE_LIMIT", "PI", "BUFFER_SIZE"]
    );
}

#[test]
fn cpp_tags_query_keeps_const_class_members_as_fields_only() {
    let source = r#"
struct Config {
  const int retries;
};
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("field_declaration_list"), "{sexp}");
    assert!(sexp.contains("field_declaration"), "{sexp}");

    assert_eq!(definition_names(source, "definition.field"), ["retries"]);
    assert!(
        !definition_names(source, "definition.constant").contains(&"retries".to_owned()),
        "const class data members must not be double-emitted as constants"
    );
}

#[test]
fn cpp_extractor_preserves_enum_member_parent_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/color.cpp"),
        r#"
namespace demo {
enum class Color {
  Red,
  Green = 2,
};
}
"#,
    )
    .expect("write color.cpp");

    let facts = build_facts(root, None).expect("extract").0;
    let color_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Enum && node.label == "Color")
        .expect("Color enum symbol");
    let red_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::EnumVariant && node.label == "Red")
        .expect("Red enumerator symbol");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Contains
                && edge.source_node_id == color_node.node_id
                && edge.target_node_id == Some(red_node.node_id)
        }),
        "expected Red enumerator to be contained by Color"
    );
}
