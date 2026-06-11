; SPUR-specific edge captures for Lua.

; ============ imports ============

((function_call
   name: (identifier) @require.name
   arguments: (arguments
     (string) @import.name)) @import
 (#eq? @require.name "require"))

; ============ call sites ============

(function_call
  name: [
    (identifier) @call.name
    (dot_index_expression
      field: (identifier) @call.name)
    (method_index_expression
      method: (identifier) @call.name)
  ]) @call
