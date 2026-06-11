; C symbol tags. Follows the tree-sitter @definition.* / @name convention.

; -------- structs / unions --------

(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(union_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

; -------- enums --------

(enum_specifier
  name: (type_identifier) @name) @definition.enum

(enumerator_list
  (enumerator
    name: (identifier) @name) @definition.enum_variant)

; -------- functions --------

(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

; -------- type aliases --------

(type_definition
  declarator: (type_identifier) @name) @definition.type_alias

; -------- preprocessor macros --------

(preproc_def
  name: (identifier) @name) @definition.macro

(preproc_function_def
  name: (identifier) @name) @definition.macro

; -------- file-scope constants --------

(translation_unit
  (declaration
    (type_qualifier)
    declarator: (init_declarator
      declarator: (identifier) @name)) @definition.constant)

(translation_unit
  (declaration
    (type_qualifier)
    declarator: (identifier) @name) @definition.constant)

; -------- fields --------

(field_declaration
  declarator: (field_identifier) @name) @definition.field
