; SPUR-specific edge captures for C.

; ============ imports ============

(preproc_include
  path: (string_literal) @import.name) @import

(preproc_include
  path: (system_lib_string) @import.name) @import

; ============ call sites ============

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (field_expression
    field: (field_identifier) @call.name)) @call

; Macro-style preprocessor invocations at file scope.
(preproc_call
  directive: (preproc_directive) @call.name) @call
