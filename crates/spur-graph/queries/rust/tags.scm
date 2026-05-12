; Follows the standard tree-sitter tags.scm convention of @definition.*
; captures for code navigation symbols.

(mod_item
  name: (identifier)) @definition.module

(struct_item
  name: (type_identifier)) @definition.struct

(enum_item
  name: (type_identifier)) @definition.enum

(trait_item
  name: (type_identifier)) @definition.trait

(impl_item
  type: (_) @name) @definition.impl

(impl_item
  body: (declaration_list
    (function_item
      name: (identifier)) @definition.method))

(function_item
  name: (identifier)) @definition.function
