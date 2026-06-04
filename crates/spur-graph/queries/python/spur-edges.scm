; SPUR-specific edge captures for Python imports and call sites.

(import_statement
  name: (dotted_name
    (identifier) @import.name)) @import

(import_statement
  name: (aliased_import
    name: (dotted_name
      (identifier) @import.name))) @import

(import_from_statement
  name: (dotted_name
    (identifier) @import.name) @import)

(import_from_statement
  name: (aliased_import
    name: (dotted_name
      (identifier) @import.name) @import))

(call
  function: [
    (identifier) @call.name
    (attribute
      attribute: (identifier) @call.name)
  ]) @call

; `class C(Base):` inheritance. Python has no `implements` keyword, so a base
; class maps to `extends`. keyword_argument (e.g. metaclass=) is not matched.
(class_definition
  superclasses: (argument_list
    [(identifier) @extends.name
     (attribute
       attribute: (identifier) @extends.name)])) @extends
