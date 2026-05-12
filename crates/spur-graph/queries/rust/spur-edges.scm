; SPUR-specific graph edges that are outside the standard tags.scm surface.

(use_declaration) @import.use_declaration

(call_expression
  function: (_)) @call.call_expression

[
  (mod_item)
  (function_item)
  (struct_item)
  (enum_item)
  (trait_item)
  (impl_item)
] @contains.parent_child
