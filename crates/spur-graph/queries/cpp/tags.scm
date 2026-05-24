; C++ symbol tags. Follows the tree-sitter @definition.* / @name convention.
; Snapshot extraction intentionally reuses this tags query; do not add a
; separate cpp/symbols.scm unless structural-vs-snapshot behavior must diverge.
;
; Tuned for production C++ codebases like DuckDB:
;   - templated and non-templated forms share patterns (template_declaration
;     wraps the inner declaration, which is what we match)
;   - out-of-line method definitions (`bool DataChunk::Verify() const`) carry
;     their qualified prefix in @name so the FQN survives without class
;     ancestry in the AST
;   - operator overloads, constructors, destructors are all captured
;   - preprocessor macros count as definitions for cross-file linking
;
; Method vs function disambiguation is handled by the Rust adapter via the
; `has_cpp_class_ancestor` predicate, mirroring the rust/python pattern.

; -------- namespaces (Module) --------

(namespace_definition
  name: (namespace_identifier) @name) @definition.module

(namespace_definition
  name: (nested_namespace_specifier
          (namespace_identifier) @name)) @definition.module

; -------- classes / structs / unions --------

(class_specifier
  name: (type_identifier) @name) @definition.class

(struct_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

(union_specifier
  name: (type_identifier) @name
  body: (_)) @definition.struct

; -------- enums (plain + scoped `enum class`) --------

(enum_specifier
  name: (type_identifier) @name) @definition.enum

(enum_specifier
  name: (qualified_identifier
          name: (type_identifier) @name)) @definition.enum

; -------- methods declared inside a class/struct body --------

(field_declaration
  (function_declarator
    declarator: (field_identifier) @name)) @definition.method

(field_declaration
  (function_declarator
    declarator: (destructor_name) @name)) @definition.method

(field_declaration
  (function_declarator
    declarator: (operator_name) @name)) @definition.method

; in-class function bodies (constructors, inline methods)
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @name)) @definition.method

(function_definition
  declarator: (function_declarator
    declarator: (destructor_name) @name)) @definition.method

(function_definition
  declarator: (function_declarator
    declarator: (operator_name) @name)) @definition.method

; -------- free functions and out-of-line method definitions --------
;
; `function_definition` with a plain identifier is a free function.
; With a `qualified_identifier`, it is an out-of-line member definition
; (e.g. `void Catalog::Initialize()`). Capturing the qualified_identifier
; as @name keeps the class prefix in the FQN without requiring AST ancestry.

(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier) @name)) @definition.method

(function_definition
  declarator: (function_declarator
    declarator: (operator_name) @name)) @definition.function

; Pointer/reference-returning out-of-line definitions wrap the function_declarator
; in a pointer_declarator / reference_declarator.
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (qualified_identifier) @name))) @definition.method

(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (qualified_identifier) @name))) @definition.method

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (identifier) @name))) @definition.function

; -------- templated function / method declarations --------
;
; Inside a class body, a templated member declaration is parsed as
;   field_declaration_list -> template_declaration -> declaration -> function_declarator
; rather than the field_declaration shape. We capture the inner function name
; here; `has_cpp_class_ancestor` reclassifies to Method when the surrounding
; container is a class/struct/union body.

(template_declaration
  (declaration
    (function_declarator
      declarator: (identifier) @name))) @definition.function

(template_declaration
  (declaration
    (function_declarator
      declarator: (field_identifier) @name))) @definition.function

(template_declaration
  (declaration
    (function_declarator
      declarator: (operator_name) @name))) @definition.function

(template_declaration
  (function_definition
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

(template_declaration
  (function_definition
    declarator: (function_declarator
      declarator: (field_identifier) @name))) @definition.function

(template_declaration
  (function_definition
    declarator: (function_declarator
      declarator: (qualified_identifier) @name))) @definition.method

; -------- type aliases --------

; `typedef Foo Bar;`
(type_definition
  declarator: (type_identifier) @name) @definition.type_alias

; `using Foo = Bar;`
(alias_declaration
  name: (type_identifier) @name) @definition.type_alias

; `namespace short = long::path;`
(namespace_alias_definition
  name: (namespace_identifier) @name) @definition.type_alias

; -------- preprocessor macros --------

(preproc_def
  name: (identifier) @name) @definition.macro

(preproc_function_def
  name: (identifier) @name) @definition.macro

; -------- fields (class data members) --------
;
; field_declaration captures both methods (handled above) and data members.
; This pattern only matches when there is no function_declarator inside,
; so methods are not double-counted.
;
; The field_identifier capture catches the simple `int x;` case. Pointer,
; reference and array member declarators wrap the field_identifier in
; another node; those are intentionally elided from v1 — the call graph
; cares about behavior, not data shape, and DuckDB's hot navigation
; targets are functions/methods.

(field_declaration
  declarator: (field_identifier) @name) @definition.field
