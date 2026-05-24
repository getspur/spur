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
      value: (_) @call.receiver
      field: (field_identifier) @call.name)
    (scoped_identifier
      path: (_) @call.scope
      name: (identifier) @call.name)
  ]) @call

; Function-value passed as the first argument to a well-known higher-order method.
; Captures `.map(name)`, `.filter(name)`, `.and_then(name)`, etc. The closed list of
; methods bounds false-positive exposure to recognized HOF positions.
((call_expression
   function: (field_expression
     field: (field_identifier) @hof_method)
   arguments: (arguments . (identifier) @reference.name))
 (#match? @hof_method "^(map|filter|for_each|and_then|or_else|unwrap_or_else|inspect|filter_map|flat_map|reduce|find_map|then|map_err|any|sort_by|all|find|position|sort_by_key|sort_unstable_by|sort_unstable_by_key|take_while|skip_while|partition|try_for_each|retain|max_by|min_by|max_by_key|min_by_key)$"))

; `fold(init, fn)`, `scan(init, fn)`, and `try_fold(init, fn)` place the callable
; in the SECOND argument position.
((call_expression
   function: (field_expression
     field: (field_identifier) @hof_method)
   arguments: (arguments (_) . (identifier) @reference.name))
 (#match? @hof_method "^(fold|scan|try_fold)$"))

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
