; Follows the standard tree-sitter tags.scm convention of @definition.*
; captures for code navigation symbols, with inner @name captures for labels.

(mod_item
  name: (identifier) @name) @definition.module

(struct_item
  name: (type_identifier) @name) @definition.struct

(enum_item
  name: (type_identifier) @name) @definition.enum

; Canonical Rust definition items not covered by the baseline tags surface.
(union_item
  name: (type_identifier) @name) @definition.struct

(field_declaration
  name: (field_identifier) @name) @definition.field

(type_item
  name: (type_identifier) @name) @definition.type_alias

(const_item
  name: (identifier) @name) @definition.constant

(static_item
  name: (identifier) @name) @definition.constant

(macro_definition
  name: (identifier) @name) @definition.macro

(trait_item
  name: (type_identifier) @name) @definition.trait

(trait_item
  body: (declaration_list
    [
      (function_signature_item
        name: (identifier) @name) @definition.method
      (function_item
        name: (identifier) @name) @definition.method
    ]))

(impl_item
  trait: (_) @impl.trait
  type: ((_) @impl.self @name)) @definition.impl

(impl_item
  !trait
  type: ((_) @impl.self @name)) @definition.impl

(impl_item
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))

(function_item
  name: (identifier) @name) @definition.function
