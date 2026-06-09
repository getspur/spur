use std::fs;

use spur_graph::{build_facts, NodeKind, RelationKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const TS_EDGES_QUERY: &str = include_str!("../queries/typescript/spur-edges.scm");

fn capture_texts(
    language: &tree_sitter::Language,
    source: &str,
    capture_name: &str,
) -> Vec<String> {
    let mut parser = Parser::new();
    parser
        .set_language(language)
        .expect("configure parser language");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(language, TS_EDGES_QUERY).expect("compile query");
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

fn expected_import_names(import_names: &[String], expected: &[&str]) {
    use std::collections::HashSet;
    let mut actual: HashSet<&str> = import_names.iter().map(String::as_str).collect();
    for path in ["./module", "./types"] {
        actual.remove(path);
    }
    let expected: HashSet<&str> = expected.iter().copied().collect();
    assert_eq!(
        actual, expected,
        "import captures missing expected names: {actual:?}"
    );
}

#[test]
fn ts_spur_edges_query_captures_import_variants_implements_extends_reexports() {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let source = r#"
import { Alpha, Beta } from "./module";
import Defaulted from "./module";
import * as All from "./module";
import type { TypeOnly } from "./types";

export { Alpha } from "./module";
export * from "./module";

class Widget extends Base implements Alpha, All.Service {}

interface Local extends Beta, All.Service {}
"#;

    let import_names = capture_texts(&language, source, "import.name");
    expected_import_names(
        &import_names,
        &["All", "Alpha", "Beta", "Defaulted", "TypeOnly"],
    );

    let implements_names = capture_texts(&language, source, "implements.name");
    assert!(
        implements_names.contains(&"Alpha".to_owned()),
        "implements captures missing Alpha: {implements_names:?}"
    );
    assert!(
        implements_names.contains(&"All.Service".to_owned()),
        "implements captures missing All.Service: {implements_names:?}"
    );

    let extends_names = capture_texts(&language, source, "extends.name");
    assert!(
        extends_names.contains(&"Base".to_owned()),
        "extends captures missing Base: {extends_names:?}"
    );

    let reexport_names = capture_texts(&language, source, "reexport.name");
    assert!(
        reexport_names.contains(&"Alpha".to_owned()),
        "reexport captures missing Alpha: {reexport_names:?}"
    );
    assert!(
        reexport_names.contains(&"*".to_owned()),
        "reexport captures missing glob: {reexport_names:?}"
    );
}

#[test]
fn tsx_spur_edges_query_captures_import_variants_implements_extends_reexports() {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let source = r#"
import { Alpha, Beta } from "./module";
import Defaulted from "./module";
import * as All from "./module";
import type { TypeOnly } from "./types";

export { Alpha } from "./module";
export * from "./module";

class Widget extends Base implements Alpha, All.Service {}

interface Local extends Beta, All.Service {}

const jsx = <div />;
"#;

    let import_names = capture_texts(&language, source, "import.name");
    expected_import_names(
        &import_names,
        &["All", "Alpha", "Beta", "Defaulted", "TypeOnly"],
    );

    let implements_names = capture_texts(&language, source, "implements.name");
    assert!(
        implements_names.contains(&"Alpha".to_owned()),
        "implements captures missing Alpha: {implements_names:?}"
    );
    assert!(
        implements_names.contains(&"All.Service".to_owned()),
        "implements captures missing All.Service: {implements_names:?}"
    );

    let reexport_names = capture_texts(&language, source, "reexport.name");
    assert!(
        reexport_names.contains(&"Alpha".to_owned()),
        "reexport captures missing Alpha: {reexport_names:?}"
    );
}

#[test]
fn ts_graph_emits_import_implements_extends_reexport_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/module.ts"),
        r#"
export interface Service {}
export interface ServiceFromNamespace {}

export class Base {}
export default class Defaulted {}
export class Wrapped {}
export const value = 1;
"#,
    )
    .expect("write module.ts");
    fs::write(
        root.join("src/types.ts"),
        "export type TypeOnly = { marker: true };\n",
    )
    .expect("write types.ts");

    fs::write(
        root.join("src/app.ts"),
        r#"
import { Service, Base } from "./module";
import Defaulted from "./module";
import * as All from "./module";
import type { TypeOnly } from "./types";

export { Base } from "./module";
export * from "./module";

class App extends Defaulted implements Service {}
class Inherited extends Base {}
interface AppInterface extends All.ServiceFromNamespace {}
"#,
    )
    .expect("write app.ts");

    let (facts, _counts) = build_facts(root, None).expect("build facts");
    let app_file_id = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.label == "src/app.ts")
        .expect("missing app.ts file")
        .node_id;

    let has_import = |target: &str, path: Option<&str>| {
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Imports
                && edge.source_node_id == app_file_id
                && edge.target_label.as_deref() == Some(target)
                && match path {
                    Some(expected) => edge.import_path.as_deref() == Some(expected),
                    None => true,
                }
        })
    };

    assert!(
        has_import("Service", Some("./module")),
        "missing imported Service edge"
    );
    assert!(
        has_import("Base", Some("./module")),
        "missing imported or re-exported Base edge"
    );
    assert!(
        has_import("Defaulted", Some("./module")),
        "missing imported default Base edge"
    );
    assert!(
        has_import("All", Some("./module")),
        "missing namespace import edge"
    );
    assert!(
        has_import("TypeOnly", Some("./types")),
        "missing type-only import edge"
    );
    assert!(
        has_import("*", Some("./module")),
        "missing re-export glob edge"
    );

    let app_class = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.label == "App")
        .expect("class App symbol");
    assert!(
        facts.edges.iter().any(|edge| {
            edge.source_node_id == app_class.node_id
                && edge.relation == RelationKind::Implements
                && edge.target_label.as_deref() == Some("Service")
        }),
        "missing App implements Service edge"
    );
    assert!(
        facts.edges.iter().any(|edge| {
            edge.source_node_id == app_class.node_id
                && edge.relation == RelationKind::Extends
                && edge.target_label.as_deref() == Some("Defaulted")
        }),
        "missing App extends Defaulted edge"
    );

    let interface_node = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Interface && node.label == "AppInterface")
        .expect("interface AppInterface symbol");
    assert!(
        facts.edges.iter().any(|edge| {
            edge.source_node_id == interface_node.node_id
                && edge.relation == RelationKind::Extends
                && edge.target_label.as_deref() == Some("All.ServiceFromNamespace")
        }),
        "missing AppInterface extends All.ServiceFromNamespace edge"
    );
}

#[test]
fn tsx_graph_emits_import_implements_extends_reexport_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(
        root.join("src/module.tsx"),
        r#"
export interface Service {}
export interface ServiceFromNamespace {}

export class Base {}
export default class Defaulted {}
"#,
    )
    .expect("write module.tsx");
    fs::write(
        root.join("src/types.ts"),
        "export type TypeOnly = { marker: true };\n",
    )
    .expect("write types.ts");

    fs::write(
        root.join("src/app.tsx"),
        r#"
import { Service } from "./module";
import Defaulted from "./module";
import * as All from "./module";
import type { TypeOnly } from "./types";

export { Service } from "./module";
export * from "./module";

const node = <div />;

class App extends Defaulted implements Service {}
interface AppInterface extends All.ServiceFromNamespace {}
"#,
    )
    .expect("write app.tsx");

    let (facts, _counts) = build_facts(root, None).expect("build facts");
    let app_file_id = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.label == "src/app.tsx")
        .expect("missing app.tsx file")
        .node_id;

    let has_import = |target: &str, path: Option<&str>| {
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Imports
                && edge.source_node_id == app_file_id
                && edge.target_label.as_deref() == Some(target)
                && match path {
                    Some(expected) => edge.import_path.as_deref() == Some(expected),
                    None => true,
                }
        })
    };

    assert!(
        has_import("Service", Some("./module")),
        "missing imported Service edge"
    );
    assert!(
        has_import("Defaulted", Some("./module")),
        "missing imported default edge"
    );
    assert!(
        has_import("All", Some("./module")),
        "missing namespace import edge"
    );
    assert!(
        has_import("TypeOnly", Some("./types")),
        "missing type-only import edge"
    );
    assert!(
        has_import("*", Some("./module")),
        "missing re-export glob edge"
    );

    let app_class = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.label == "App")
        .expect("class App symbol");
    assert!(
        facts.edges.iter().any(|edge| {
            edge.source_node_id == app_class.node_id
                && edge.relation == RelationKind::Extends
                && edge.target_label.as_deref() == Some("Defaulted")
        }),
        "missing App extends Defaulted edge"
    );
    assert!(
        facts.edges.iter().any(|edge| {
            edge.source_node_id == app_class.node_id
                && edge.relation == RelationKind::Implements
                && edge.target_label.as_deref() == Some("Service")
        }),
        "missing App implements Service edge"
    );
}
