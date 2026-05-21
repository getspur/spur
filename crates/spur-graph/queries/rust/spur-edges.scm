; SPUR-specific graph edges that are outside the standard tags.scm surface.

(use_declaration
  argument: (identifier) @import.name) @import

(use_declaration
  argument: (scoped_identifier
    name: (identifier) @import.name)) @import

(use_declaration
  argument: (scoped_use_list
    list: (use_list
      (identifier) @import.name))) @import

(use_declaration
  argument: (scoped_use_list
    list: (use_list
      (scoped_identifier
        name: (identifier) @import.name)))) @import

(call_expression
  function: [
    (identifier) @call.name
    (field_expression
      field: (field_identifier) @call.name)
    (scoped_identifier
      name: (identifier) @call.name)
  ]) @call

; Macro-body call sites. tree-sitter-rust parses macro arguments as
; flat `token_tree` (see tree-sitter-rust node-types.json: token_tree
; accepts only identifier/_literal/token_tree/etc., not call_expression),
; so `(call_expression …)` above never matches inside json!{}, format!(),
; tracing::info!(), etc. The two patterns below capture name+paren-args
; pairs inside macro bodies. The `"(" ")"` anchors constrain the trailing
; token_tree to a parenthesized one, filtering indexing (`out[0]`) and
; block (`else { 1 }`) false positives while keeping real call shapes.
; Verified by crates/spur-graph/tests/rust_macro_token_tree_query.rs.
(token_tree
  (identifier) @call.name
  .
  (token_tree "(" ")")) @call

(token_tree
  (scoped_identifier
    name: (identifier) @call.name)
  .
  (token_tree "(" ")")) @call
