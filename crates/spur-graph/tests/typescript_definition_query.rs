use std::{fs, path::Path};

use pretty_assertions::assert_eq;
use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::BytesExtractor;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const TYPESCRIPT_TAGS_QUERY: &str = include_str!("../queries/typescript/tags.scm");

fn parse(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn root_sexp(source: &str) -> String {
    parse(source).root_node().to_sexp()
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let tree = parse(source);
    let query = Query::new(&language, TYPESCRIPT_TAGS_QUERY).expect("compile query");
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
fn typescript_tags_query_captures_enum_members() {
    let source = r#"
enum Mode {
  Auto,
  Manual = "manual",
}
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("enum_body"), "{sexp}");
    assert!(sexp.contains("enum_assignment"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.enum_variant"),
        ["Auto", "Manual"]
    );
}

#[test]
fn typescript_tags_query_captures_class_fields_without_arrow_double_capture() {
    let source = r#"
class Runner {
  status: string;
  count = 1;
  loader = () => 1;
}
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("public_field_definition"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.field"),
        ["status", "count"]
    );
    assert!(
        definition_names(source, "definition.function").contains(&"loader".to_owned()),
        "function-valued class fields should be function definitions, not fields"
    );
}

#[test]
fn typescript_tags_query_captures_top_level_non_function_constants() {
    let source = r#"
const LIMIT = 5;
const ROUTE = prefix + "/run";
const makeRunner = () => new Runner();
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("lexical_declaration"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.constant"),
        ["LIMIT", "ROUTE"]
    );
    assert!(
        !definition_names(source, "definition.constant").contains(&"makeRunner".to_owned()),
        "function-valued const bindings must remain function definitions"
    );
}

#[test]
fn typescript_tags_query_captures_exported_non_function_constants() {
    let source = r#"
export const SETTINGS = makeSettings({ mode: "fast" });
export const buildRunner = () => new Runner();
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("export_statement"), "{sexp}");
    assert!(sexp.contains("lexical_declaration"), "{sexp}");

    assert_eq!(
        definition_names(source, "definition.constant"),
        ["SETTINGS"]
    );
    assert!(
        !definition_names(source, "definition.constant").contains(&"buildRunner".to_owned()),
        "exported function-valued const bindings must remain function definitions"
    );
    assert!(
        definition_names(source, "definition.function").contains(&"buildRunner".to_owned()),
        "exported function-valued const bindings should still be function definitions"
    );
}

#[test]
fn typescript_extractor_preserves_enum_and_field_parent_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    let source = br#"
enum Mode {
  Auto,
  Manual = "manual",
}

class Runner {
  status: string;
  count = 1;
  loader = () => 1;
}

const LIMIT = 5;
"#;
    fs::write(root.join("src/app.ts"), source).expect("write app.ts");

    let facts = build_facts(root, None).expect("extract").0;
    let mode_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Enum && node.label == "Mode")
        .expect("Mode enum symbol");
    let auto_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::EnumVariant && node.label == "Auto")
        .expect("Auto enum variant symbol");
    let runner_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.label == "Runner")
        .expect("Runner class symbol");
    let status_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Field && node.label == "status")
        .expect("status field symbol");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Contains
                && edge.source_node_id == mode_node.node_id
                && edge.target_node_id == Some(auto_node.node_id)
        }),
        "expected Auto enum member to be contained by Mode"
    );
    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Contains
                && edge.source_node_id == runner_node.node_id
                && edge.target_node_id == Some(status_node.node_id)
        }),
        "expected status field to be contained by Runner"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Constant && node.label == "LIMIT"),
        "expected LIMIT constant symbol"
    );

    let mut extractor = BytesExtractor::for_language(Language::TypeScript).expect("extractor");
    let symbols = extractor
        .extract(Path::new("src/app.ts"), source)
        .expect("extract symbols");
    assert!(
        symbols.iter().any(|symbol| {
            symbol.symbol_kind == "function"
                && symbol.entity_name == "loader"
                && symbol.enclosing_scope.as_deref() == Some("Runner")
        }),
        "expected function-valued class field to be emitted as a scoped function"
    );
    assert!(
        !symbols.iter().any(|symbol| {
            symbol.symbol_kind == "field"
                && symbol.entity_name == "loader"
                && symbol.enclosing_scope.as_deref() == Some("Runner")
        }),
        "function-valued class field must not be double-emitted as a field"
    );
}
