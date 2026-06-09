; SPUR-specific graph edges that are outside the standard tags.scm surface.
; Shared by both the TypeScript and TSX grammars, so it must only reference
; nodes common to both. JSX nodes live in `jsx-edges.scm` (TSX only).

(import_statement
  source: (string
    (string_fragment) @import.name @import.path) @import)

(import_statement
  (import_clause
    (identifier) @import.name)
  source: (string
    (string_fragment) @import.path)) @import

(import_statement
  (import_clause
    (namespace_import
      (identifier) @import.name))
  source: (string
    (string_fragment) @import.path)) @import

(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name)))
  source: (string
    (string_fragment) @import.path)) @import

(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @reexport.name))
  source: (string
    (string_fragment) @reexport.path)) @reexport

(export_statement
  (namespace_export
    (identifier) @reexport.name)
  source: (string
    (string_fragment) @reexport.path)) @reexport

(export_statement
  "*" @reexport.name
  source: (string
    (string_fragment) @reexport.path)) @reexport

(call_expression
  function: [
    (identifier) @call.name
    (member_expression
      property: (property_identifier) @call.name)
  ]) @call

; Constructor instantiation: `new Foo()` becomes a Calls edge to `Foo`.
(new_expression
  constructor: (identifier) @call.name) @call

; `class C implements I` emits an Implements edge to each interface.
(class_declaration
  (class_heritage
    (implements_clause
      [
        (type_identifier) @implements.name
        (type
          (type_identifier) @implements.name)
        (type
          (primary_type
            (type_identifier) @implements.name))
        (type
          (generic_type
            (type_identifier) @implements.name))
        (type
          (primary_type
              (generic_type
                (type_identifier) @implements.name)))
        (type
          (nested_type_identifier) @implements.name)
        (type
          (primary_type
            (nested_type_identifier) @implements.name))
        (type
          (generic_type
            (nested_type_identifier) @implements.name))
        (nested_type_identifier) @implements.name
        (generic_type
          (nested_type_identifier) @implements.name)
        (generic_type
          (type_identifier) @implements.name)
        (primary_type
          (type_identifier) @implements.name)
        (primary_type
          (nested_type_identifier) @implements.name)
      ]))) @implements

; `class C extends B` emits an Extends edge to the base class.
(class_declaration
  (class_heritage
    (extends_clause
      value: [
        (identifier) @extends.name
        (type (type_identifier) @extends.name)
        (primary_type (type_identifier) @extends.name)
        (member_expression
          property: (property_identifier) @extends.name)
      ]))) @extends

; `interface I extends J` emits an Extends edge between interfaces.
(interface_declaration
  (extends_type_clause
    [
      (type_identifier) @extends.name
      (generic_type
        (type_identifier) @extends.name)
      (nested_type_identifier) @extends.name
      (generic_type
        (nested_type_identifier) @extends.name)
    ])) @extends
