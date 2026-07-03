use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const GO_TAGS_QUERY: &str = include_str!("../queries/go/tags.scm");

const GO_FIXTURE: &str = r#"
package geometry

import "fmt"

import (
	"net/http"
	str "strings"
)

const Origin, Unit = 0, 1

const MaxPoints = 128

var Scratch = 0

type Point struct {
	X, Y int
	Label string
}

type Shape interface {
	Area() int
}

type Meters = int

type Handle int

func (p Point) Scale(factor int) Point {
	return Point{X: p.X * factor, Y: p.Y * factor}
}

func Add(a int, b int) int {
	const local = 10
	fmt.Println(str.ToUpper("add"))
	_ = http.StatusOK
	return a + b + local
}

func MakePoint() Point {
	p := Point{X: 1, Y: 2}
	return p.Scale(Add(1, 2))
}
"#;

fn parse_go(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn definition_names(source: &str, definition_capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let tree = parse_go(source);
    let query = Query::new(&language, GO_TAGS_QUERY).expect("compile query");
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

fn build_go_fixture(source: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("geometry.go"), source).expect("write geometry.go");
    build_facts(dir.path(), None).expect("extract").0
}

#[test]
fn go_fixture_parses_without_errors() {
    let tree = parse_go(GO_FIXTURE);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn go_tags_query_captures_functions_and_methods() {
    assert_eq!(
        definition_names(GO_FIXTURE, "definition.function"),
        ["Add", "MakePoint"]
    );
    let mut methods = definition_names(GO_FIXTURE, "definition.method");
    methods.sort();
    assert_eq!(methods, ["Area", "Scale"]);
}

#[test]
fn go_tags_query_fans_out_multi_name_const_spec() {
    assert_eq!(
        definition_names(GO_FIXTURE, "definition.constant"),
        ["Origin", "Unit", "MaxPoints"]
    );
}

#[test]
fn go_tags_query_captures_types_and_fields() {
    assert_eq!(definition_names(GO_FIXTURE, "definition.struct"), ["Point"]);
    assert_eq!(
        definition_names(GO_FIXTURE, "definition.interface"),
        ["Shape"]
    );
    assert_eq!(
        definition_names(GO_FIXTURE, "definition.field"),
        ["X", "Y", "Label"]
    );
    assert_eq!(
        definition_names(GO_FIXTURE, "definition.module"),
        ["geometry"]
    );
}

#[test]
fn go_tags_query_skips_locals_and_var_specs() {
    let names = definition_names(GO_FIXTURE, "definition.constant");
    assert!(
        !names.contains(&"local".to_owned()),
        "function-local const must not be captured: {names:?}"
    );
    let source =
        "package p\n\nfunc f() {\n\tconst inner = 1\n\tvar v = 2\n\t_ = inner\n\t_ = v\n}\n";
    assert!(definition_names(source, "definition.constant").is_empty());
}

#[test]
fn go_extractor_builds_symbols_with_expected_kinds() {
    let facts = build_go_fixture(GO_FIXTURE);
    let has_node = |kind: NodeKind, label: &str| {
        facts
            .nodes
            .iter()
            .any(|node| node.kind == kind && node.label == label)
    };

    assert!(has_node(NodeKind::Module, "geometry"));
    assert!(has_node(NodeKind::Function, "Add"));
    assert!(has_node(NodeKind::Function, "MakePoint"));
    assert!(has_node(NodeKind::Method, "Scale"));
    assert!(has_node(NodeKind::Method, "Area"));
    assert!(has_node(NodeKind::Struct, "Point"));
    assert!(has_node(NodeKind::Interface, "Shape"));
    assert!(has_node(NodeKind::TypeAlias, "Meters"));
    assert!(has_node(NodeKind::TypeAlias, "Handle"));
    assert!(has_node(NodeKind::Field, "X"));
    assert!(has_node(NodeKind::Field, "Y"));
    assert!(has_node(NodeKind::Field, "Label"));
    assert!(has_node(NodeKind::Constant, "Origin"));
    assert!(has_node(NodeKind::Constant, "Unit"));
    assert!(has_node(NodeKind::Constant, "MaxPoints"));

    assert!(
        !facts.nodes.iter().any(|node| node.label == "local"),
        "function-local const must not become a symbol"
    );
    assert!(
        !facts.nodes.iter().any(|node| node.label == "Scratch"),
        "top-level var is out of scope for this phase"
    );
    assert!(
        !facts
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::TypeAlias && node.label == "Point"),
        "struct type_spec must not double-emit as a type alias"
    );
}

#[test]
fn go_extractor_builds_import_edges_with_clean_paths() {
    let facts = build_go_fixture(GO_FIXTURE);
    let import_edge = |target: &str| {
        facts
            .edges
            .iter()
            .find(|edge| {
                edge.relation == RelationKind::Imports
                    && edge.target_label.as_deref() == Some(target)
            })
            .unwrap_or_else(|| panic!("missing import edge for {target}"))
    };

    assert_eq!(import_edge("fmt").import_path.as_deref(), Some("fmt"));
    assert_eq!(
        import_edge("net/http").import_path.as_deref(),
        Some("net/http")
    );
    assert_eq!(import_edge("str").import_path.as_deref(), Some("strings"));
}

#[test]
fn go_extractor_builds_call_and_construct_edges() {
    let facts = build_go_fixture(GO_FIXTURE);
    let has_edge = |relation: RelationKind, target: &str| {
        facts
            .edges
            .iter()
            .any(|edge| edge.relation == relation && edge.target_label.as_deref() == Some(target))
    };

    assert!(has_edge(RelationKind::Calls, "Add"));
    assert!(has_edge(RelationKind::Calls, "Scale"));
    assert!(has_edge(RelationKind::Calls, "Println"));
    assert!(
        has_edge(RelationKind::Constructs, "Point"),
        "composite literal of a struct should resolve to constructs; edges: {:?}",
        facts
            .edges
            .iter()
            .map(|edge| (edge.relation, edge.target_label.clone()))
            .collect::<Vec<_>>()
    );
    assert!(!has_edge(RelationKind::Constructs, "Add"));

    let method_call = facts
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::Calls && edge.target_label.as_deref() == Some("Println")
        })
        .expect("qualified call edge");
    assert_eq!(method_call.receiver_text.as_deref(), Some("fmt"));
}
