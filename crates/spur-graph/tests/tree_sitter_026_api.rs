//! Sat-model witness for the Tree-sitter 0.26 upgrade (`sol_6c6c4aa9d63f4163`).
//!
//! `configuration.attribute_allowed_pair` requires spur-graph `child_index=u32`
//! to match tree-sitter 0.26.13 (`Node::child` / `named_child` take `u32`).
//! On 0.25 this file fails to compile (`expected usize, found u32`).

use tree_sitter::Parser;

#[test]
fn tree_sitter_026_child_index_is_u32() {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser
        .parse("fn main() {}", None)
        .expect("parse rust source");
    let root = tree.root_node();

    // Explicit u32: the sat-model child_index assignment, not an inferred 0.
    let _ = root.child(0u32);
    let named = root
        .named_child(0u32)
        .expect("source_file has a named child");
    assert_eq!(named.kind(), "function_item");
}
