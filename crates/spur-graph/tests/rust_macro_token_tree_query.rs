use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

const PROPOSED_QUERY: &str = r#"
(token_tree
  (identifier) @call.name
  .
  (token_tree)) @call

(token_tree
  (scoped_identifier
    name: (identifier) @call.name)
  .
  (token_tree)) @call
"#;

const PARENTHESIZED_ARG_QUERY: &str = r#"
(token_tree
  (identifier) @call.name
  .
  (token_tree "(" ")")) @call

(token_tree
  (scoped_identifier
    name: (identifier) @call.name)
  .
  (token_tree "(" ")")) @call
"#;

fn call_names(query_source: &str, source: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == "call.name" {
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
fn proposed_macro_token_tree_query_captures_macro_body_calls() {
    let source = r#"fn caller() { json!({ "x": mermaid_subgraph(&view.nodes, &view.edges), "y": Type::bar(2) }); }"#;

    let names = call_names(PROPOSED_QUERY, source);

    assert!(names.contains(&"mermaid_subgraph".to_string()));
    assert!(names.contains(&"bar".to_string()));
}

#[test]
fn parenthesized_arg_query_filters_common_macro_token_false_positives() {
    let source = r#"fn caller() { json!({ "x": mermaid_subgraph(&view.nodes, &view.edges), "idx": out[0].name, "kw": if ok { 1 } else { 0 }, "variant": Some(Action::InspectPlan { plan_id }) }); }"#;

    let names = call_names(PARENTHESIZED_ARG_QUERY, source);

    assert!(names.contains(&"mermaid_subgraph".to_string()));
    assert!(names.contains(&"Some".to_string()));
    assert!(!names.contains(&"out".to_string()));
    assert!(!names.contains(&"else".to_string()));
    assert!(!names.contains(&"InspectPlan".to_string()));
}
