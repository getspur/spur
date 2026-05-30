use std::path::Path;

use pretty_assertions::assert_eq;
use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::BytesExtractor;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const PYTHON_TAGS_QUERY: &str = include_str!("../queries/python/tags.scm");

fn parse(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn root_sexp(source: &str) -> String {
    parse(source).root_node().to_sexp()
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let tree = parse(source);
    let query = Query::new(&language, PYTHON_TAGS_QUERY).expect("compile query");
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
fn python_tags_query_captures_module_level_constants() {
    let source = r#"
CONFIG = 1

class User:
    ROLE = "admin"
"#;
    let sexp = root_sexp(source);
    assert!(sexp.contains("expression_statement"), "{sexp}");
    assert!(sexp.contains("assignment"), "{sexp}");

    assert_eq!(definition_names(source, "definition.constant"), ["CONFIG"]);
}

#[test]
fn python_snapshot_extractor_reuses_tags_for_constants() {
    let source = br#"
CONFIG = 1

class User:
    ROLE = "admin"
"#;
    let mut extractor = BytesExtractor::for_language(Language::Python).expect("extractor");
    let symbols = extractor
        .extract(Path::new("src/app.py"), source)
        .expect("extract symbols");

    assert!(
        symbols.iter().any(|symbol| {
            symbol.symbol_kind == "constant"
                && symbol.entity_name == "CONFIG"
                && symbol.enclosing_scope.is_none()
        }),
        "expected module-level CONFIG constant in snapshot extraction"
    );
    assert!(
        !symbols
            .iter()
            .any(|symbol| symbol.symbol_kind == "constant" && symbol.entity_name == "ROLE"),
        "class assignments must not match the module-level canonical constant pattern"
    );
}
