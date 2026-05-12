; SPUR-specific graph edges that are outside the standard tags.scm surface.

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
