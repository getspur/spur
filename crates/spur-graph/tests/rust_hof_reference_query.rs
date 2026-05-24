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
fn rust_hof_reference_query_captures_additional_iterator_methods() {
    let source = r#"
fn caller(mut rows: Vec<Row>, items: Vec<Item>) {
    let _ = items.iter().all(all_ready);
    let _ = items.iter().find(find_match);
    let _ = items.iter().position(find_position);
    let _ = items.iter().skip_while(skip_pending);
    let _ = items.iter().take_while(take_prefix);
    let _ = items.iter().scan(seed, scan_step);
    let _ = items.iter().partition(partition_open);
    let _ = items.iter().try_fold(init, try_accumulate);
    let _ = items.iter().try_for_each(write_item);
    rows.sort_by_key(row_key);
    rows.sort_unstable_by(compare_rows);
    rows.sort_unstable_by_key(row_key_unstable);
    rows.retain(keep_row);
    let _ = rows.iter().max_by(compare_high);
    let _ = rows.iter().min_by(compare_low);
    let _ = rows.iter().max_by_key(score_row);
    let _ = rows.iter().min_by_key(rank_row);
}
"#;

    let names = reference_names(source);

    assert_eq!(
        names,
        vec![
            "all_ready",
            "find_match",
            "find_position",
            "skip_pending",
            "take_prefix",
            "scan_step",
            "partition_open",
            "try_accumulate",
            "write_item",
            "row_key",
            "compare_rows",
            "row_key_unstable",
            "keep_row",
            "compare_high",
            "compare_low",
            "score_row",
            "rank_row",
        ]
    );
    assert!(!names.contains(&"seed".to_string()));
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
