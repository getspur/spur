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

; Bare function values passed to well-known Python higher-order functions.
; The closed list constrains references to positions where a callable is
; expected, and the `(identifier)` shape excludes lambdas and inline calls.
((call
   function: (identifier) @hof_function
   arguments: (argument_list . (identifier) @reference.name))
 (#match? @hof_function "^(map|filter|reduce)$"))

; Key functions passed as keyword arguments to builtins that accept `key=`.
((call
   function: (identifier) @hof_function
   arguments: (argument_list
     (keyword_argument
       name: (identifier) @hof_keyword
       value: (identifier) @reference.name)))
 (#match? @hof_function "^(sorted|min|max)$")
 (#match? @hof_keyword "^key$"))

; In-place list sorting also accepts a `key=` callable.
((call
   function: (attribute
     attribute: (identifier) @hof_method)
   arguments: (argument_list
     (keyword_argument
       name: (identifier) @hof_keyword
       value: (identifier) @reference.name)))
 (#match? @hof_method "^sort$")
 (#match? @hof_keyword "^key$"))

; `class C(Base):` inheritance. Python has no `implements` keyword, so a base
; class maps to `extends`. keyword_argument (e.g. metaclass=) is not matched.
(class_definition
  superclasses: (argument_list
    [(identifier) @extends.name
     (attribute
       attribute: (identifier) @extends.name)])) @extends
