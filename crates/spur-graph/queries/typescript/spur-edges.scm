; SPUR-specific graph edges that are outside the standard tags.scm surface.
; Shared by both the TypeScript and TSX grammars, so it must only reference
; nodes common to both. JSX nodes live in `jsx-edges.scm` (TSX only).

(import_statement
  source: (string
    (string_fragment) @import.name) @import)

(import_specifier
  name: (identifier) @import.name) @import

(call_expression
  function: [
    (identifier) @call.name
    (member_expression
      property: (property_identifier) @call.name)
  ]) @call

; Constructor instantiation: `new Foo()` becomes a Calls edge to `Foo`.
(new_expression
  constructor: (identifier) @call.name) @call
