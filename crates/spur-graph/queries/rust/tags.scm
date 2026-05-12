; Follows the standard tree-sitter tags.scm convention of @definition.*
; captures for code navigation symbols, with inner @name captures for labels.

(mod_item
  name: (identifier) @name) @definition.module

(struct_item
  name: (type_identifier) @name) @definition.struct

(enum_item
  name: (type_identifier) @name) @definition.enum

(trait_item
  name: (type_identifier) @name) @definition.trait

(impl_item
  type: (_) @name) @definition.impl

(impl_item
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))

(function_item
  name: (identifier) @name) @definition.function
