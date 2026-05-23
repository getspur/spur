use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

const RUST_SPUR_EDGES_QUERY: &str = include_str!("../queries/rust/spur-edges.scm");

fn reference_names(source: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, RUST_SPUR_EDGES_QUERY).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == "reference.name" {
            names.push(
                capture
                    .node
                    .utf8_text(source.as_bytes())
                    .expect("capture text")
                    .to_string(),
            );
        }
    }

    names
}

#[test]
fn rust_hof_reference_query_captures_map_first_argument() {
    let source = r#"
fn caller(items: Vec<Edge>) -> Vec<Row> {
    items.into_iter().map(edge_row).collect()
}
"#;

    let names = reference_names(source);

    assert_eq!(names, vec!["edge_row"]);
}

#[test]
fn rust_hof_reference_query_captures_fold_second_argument_only() {
    let source = r#"
fn caller(items: Vec<i32>) -> usize {
    items.into_iter().fold(init, count_fn)
}
"#;

    let names = reference_names(source);

    assert_eq!(names, vec!["count_fn"]);
    assert!(!names.contains(&"init".to_string()));
}

#[test]
fn rust_hof_reference_query_ignores_plain_function_first_argument() {
    let source = r#"
fn caller(local_var: i32) {
    foo(local_var);
}
"#;

    let names = reference_names(source);

    assert!(names.is_empty());
}

#[test]
fn rust_hof_reference_query_ignores_unknown_method_first_argument() {
    let source = r#"
fn caller(items: Vec<i32>) {
    items.into_iter().unknown_hof(my_fn);
}
"#;

    let names = reference_names(source);

    assert!(names.is_empty());
}
