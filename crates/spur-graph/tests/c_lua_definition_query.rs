use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const C_TAGS_QUERY: &str = include_str!("../queries/c/tags.scm");
const LUA_TAGS_QUERY: &str = include_str!("../queries/lua/tags.scm");

fn parse_c(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn parse_lua(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_lua::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn definition_names(
    language: &tree_sitter::Language,
    tree: &tree_sitter::Tree,
    source: &str,
    query_source: &str,
    definition_capture_name: &str,
) -> Vec<String> {
    let query = Query::new(language, query_source).expect("compile query");
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
fn c_tags_query_captures_core_symbol_surface() {
    let source = r#"
#include <stdio.h>
#define LIMIT 8

const int VERSION = 1;

typedef unsigned long SpurId;

struct Entry {
    int id;
};

enum Mode {
    ModeRead,
    ModeWrite = 2,
};

int run(struct Entry *entry) {
    printf("%d", entry->id);
    return entry->id;
}
"#;
    let language: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
    let tree = parse_c(source);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );

    assert_eq!(
        definition_names(
            &language,
            &tree,
            source,
            C_TAGS_QUERY,
            "definition.function"
        ),
        ["run"]
    );
    assert_eq!(
        definition_names(&language, &tree, source, C_TAGS_QUERY, "definition.struct"),
        ["Entry"]
    );
    assert_eq!(
        definition_names(
            &language,
            &tree,
            source,
            C_TAGS_QUERY,
            "definition.enum_variant"
        ),
        ["ModeRead", "ModeWrite"]
    );
    assert_eq!(
        definition_names(&language, &tree, source, C_TAGS_QUERY, "definition.field"),
        ["id"]
    );
}

#[test]
fn lua_tags_query_captures_functions_and_methods() {
    let source = r#"
local helper = require("helper")

function top()
  helper.run()
end

function service:start()
  top()
end

local assigned = function()
  return top()
end

return {
  make = function()
    return assigned()
  end
}
"#;
    let language: tree_sitter::Language = tree_sitter_lua::LANGUAGE.into();
    let tree = parse_lua(source);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );

    assert_eq!(
        definition_names(
            &language,
            &tree,
            source,
            LUA_TAGS_QUERY,
            "definition.function"
        ),
        ["top", "assigned", "make"]
    );
    assert_eq!(
        definition_names(
            &language,
            &tree,
            source,
            LUA_TAGS_QUERY,
            "definition.method"
        ),
        ["start"]
    );
}

#[test]
fn c_extractor_builds_symbols_imports_and_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/main.c"),
        r#"
#include <stdio.h>

const int VERSION = 1;

struct Entry {
    int id;
};

int helper(struct Entry *entry) {
    return entry->id;
}

int main(void) {
    struct Entry entry = { .id = VERSION };
    return helper(&entry);
}
"#,
    )
    .expect("write main.c");

    let facts = build_facts(root, None).expect("extract").0;

    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Struct && node.label == "Entry"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Constant && node.label == "VERSION"));
    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Imports
            && edge.target_label.as_deref() == Some("<stdio.h>")));
    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Calls
            && edge.target_label.as_deref() == Some("helper")));
}

#[test]
fn lua_extractor_builds_symbols_imports_and_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/main.lua"),
        r#"
local helper = require("helper")

function top()
  return helper.run()
end

function service:start()
  return top()
end
"#,
    )
    .expect("write main.lua");

    let facts = build_facts(root, None).expect("extract").0;

    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Function && node.label == "top"));
    assert!(facts
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Method && node.label == "start"));
    assert!(facts
        .edges
        .iter()
        .any(|edge| edge.relation == RelationKind::Imports
            && edge.target_label.as_deref() == Some("\"helper\"")));
    assert!(facts.edges.iter().any(|edge| {
        edge.relation == RelationKind::Calls && edge.target_label.as_deref() == Some("top")
    }));
}
