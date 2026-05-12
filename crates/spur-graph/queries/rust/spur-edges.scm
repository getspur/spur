; SPUR-specific graph edges that are outside the standard tags.scm surface.

(use_declaration
  argument: (identifier) @import.name) @import

(use_declaration
  argument: (scoped_identifier
    name: (identifier) @import.name)) @import

(use_declaration
  argument: (scoped_use_list
    list: (use_list
      (identifier) @import.name))) @import

(use_declaration
  argument: (scoped_use_list
    list: (use_list
      (scoped_identifier
        name: (identifier) @import.name)))) @import

(call_expression
  function: [
    (identifier) @call.name
    (field_expression
      field: (field_identifier) @call.name)
    (scoped_identifier
      name: (identifier) @call.name)
  ]) @call
