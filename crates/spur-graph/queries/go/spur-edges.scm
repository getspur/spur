; SPUR-specific graph edges for Go outside the standard tags.scm surface.

(import_spec
  !name
  path: [
    (interpreted_string_literal
      (interpreted_string_literal_content) @import.name @import.path)
    (raw_string_literal
      (raw_string_literal_content) @import.name @import.path)
  ]) @import

(import_spec
  name: (package_identifier) @import.name
  path: [
    (interpreted_string_literal
      (interpreted_string_literal_content) @import.path)
    (raw_string_literal
      (raw_string_literal_content) @import.path)
  ]) @import

(import_spec
  name: [(dot) (blank_identifier)]
  path: [
    (interpreted_string_literal
      (interpreted_string_literal_content) @import.name @import.path)
    (raw_string_literal
      (raw_string_literal_content) @import.name @import.path)
  ]) @import

(call_expression
  function: [
    (identifier) @call.name
    (selector_expression
      operand: (_) @call.receiver
      field: (field_identifier) @call.name)
  ]) @call

(composite_literal
  type: (type_identifier) @call.name) @call

(composite_literal
  type: (qualified_type
    package: (package_identifier) @call.scope
    name: (type_identifier) @call.name)) @call

(composite_literal
  type: (generic_type
    type: (type_identifier) @call.name)) @call
