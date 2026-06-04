; SPUR-specific edge captures for C++.
;
; Imports, namespace usage, and call sites. Tuned for codebases that lean
; heavily on `#include`, deep namespace nesting (`duckdb::common::types::...`),
; method dispatch via `.`/`->`/`::`, and macros that wrap call sites
; (`D_ASSERT(...)`, `Catch::SECTION(...)`).

; ============ imports ============

; #include "foo.hpp" / #include <vector>
(preproc_include
  path: (string_literal) @import.name) @import

(preproc_include
  path: (system_lib_string) @import.name) @import

; using namespace foo;          (namespace-wide import)
; using namespace foo::bar;
(using_declaration
  (identifier) @import.name) @import

(using_declaration
  (qualified_identifier) @import.name) @import

; Specific symbol pull-ins: `using std::vector;`
(using_declaration
  (qualified_identifier
    name: (identifier) @import.name)) @import

; C++20 modules (`import foo;`) are not surfaced by tree-sitter-cpp 0.23's
; grammar yet; revisit once the upstream grammar exposes `import_declaration`.

; ============ call sites ============
;
; Every call_expression. The @call.name capture extracts the trailing
; identifier; the resolver maps it to a node by name within the file's
; visible scope. Qualified calls (`Catalog::GetEntry(...)`) keep only the
; rightmost name to match the resolver's name index — fully-qualified
; resolution would need scope/visibility analysis which is out of scope
; for syntax-only extraction.

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (field_expression
    field: (field_identifier) @call.name)) @call

(call_expression
  function: (qualified_identifier
    name: (identifier) @call.name)) @call

; Pointer-to-member function call: `(obj->*pmf)(args)`.
(call_expression
  function: (parenthesized_expression
    (binary_expression
      right: (identifier) @call.name))) @call

; Template call: `MakeShared<Foo>(...)` — extract the template's bare name.
(call_expression
  function: (template_function
    name: (identifier) @call.name)) @call

; Bare function/functor values passed to a closed list of STL higher-order
; algorithms. Qualified callable arguments capture only the rightmost name,
; mirroring normal C++ call extraction.
((call_expression
   function: (qualified_identifier
     scope: (namespace_identifier) @hof_scope
     name: (identifier) @hof_algorithm)
   arguments: (argument_list
     .
     (_)
     .
     (_)
     .
     [(identifier) @reference.name
      (qualified_identifier
        name: (identifier) @reference.name)]))
 (#match? @hof_scope "^std$")
 (#match? @hof_algorithm "^(for_each|sort|find_if|remove_if|count_if)$"))

((call_expression
   function: (qualified_identifier
     scope: (namespace_identifier) @hof_scope
     name: (identifier) @hof_algorithm)
   arguments: (argument_list
     .
     (_)
     .
     (_)
     .
     (_)
     .
     [(identifier) @reference.name
      (qualified_identifier
        name: (identifier) @reference.name)]))
 (#match? @hof_scope "^std$")
 (#match? @hof_algorithm "^(transform|accumulate)$"))

((call_expression
   function: (qualified_identifier
     scope: (namespace_identifier) @hof_scope
     name: (identifier) @hof_algorithm)
   arguments: (argument_list
     .
     (_)
     .
     (_)
     .
     (_)
     .
     (_)
     .
     [(identifier) @reference.name
      (qualified_identifier
        name: (identifier) @reference.name)]))
 (#match? @hof_scope "^std$")
 (#match? @hof_algorithm "^transform$"))

; ============ macro-wrapped call sites ============
;
; `preproc_call` captures `IDENT(args)` shapes at the file/namespace level
; (e.g. DuckDB's `STATIC_ASSERT(...)`, `INSTANTIATE_TYPE(...)`). These look
; like calls to downstream consumers and resolve like any other name.

(preproc_call
  directive: (preproc_directive) @call.name) @call

; ============ inheritance ============
;
; C++ base classes: `class D : public Base` / `struct D : Base`. C++ has no
; syntactic interface, so inheritance maps to `extends` (not `implements`).

(class_specifier
  (base_class_clause
    [(type_identifier) @extends.name
     (qualified_identifier name: (type_identifier) @extends.name)])) @extends

(struct_specifier
  (base_class_clause
    [(type_identifier) @extends.name
     (qualified_identifier name: (type_identifier) @extends.name)])) @extends
