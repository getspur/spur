; Follows the standard tree-sitter tags.scm convention of @definition.*
; captures for code navigation symbols, with inner @name captures for labels.

(class_declaration
  name: (type_identifier) @name) @definition.class

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

(class
  name: (_) @name) @definition.class

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(interface_declaration
  body: (interface_body
    (method_signature
      name: (property_identifier) @name) @definition.method))

(abstract_method_signature
  name: (property_identifier) @name) @definition.method

(enum_declaration
  name: (identifier) @name) @definition.enum

(enum_declaration
  body: (enum_body
    (property_identifier) @name @definition.enum_variant))

(enum_declaration
  body: (enum_body
    (enum_assignment
      name: (property_identifier) @name) @definition.enum_variant))

(function_declaration
  name: (identifier) @name) @definition.function

(function_expression
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

(generator_function
  name: (identifier) @name) @definition.function

(function_signature
  name: (identifier) @name) @definition.function

(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(public_field_definition
  name: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(assignment_expression
  left: [
    (identifier) @name
    (member_expression
      property: (property_identifier) @name)
  ]
  right: [(arrow_function) (function_expression)]) @definition.function

(pair
  key: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(method_definition
  name: (property_identifier) @name) @definition.method

(public_field_definition
  name: (property_identifier) @name
  !value) @definition.field

(public_field_definition
  name: (property_identifier) @name
  value: [
    (number)
    (string)
    (identifier)
    (member_expression)
    (call_expression)
    (new_expression)
    (object)
    (array)
  ]) @definition.field

(module
  name: (identifier) @name) @definition.module

(type_alias_declaration
  name: (type_identifier) @name) @definition.type_alias

(program
  (lexical_declaration
    "const"
    (variable_declarator
      name: (identifier) @name
      value: [
        (number)
        (string)
        (identifier)
        (member_expression)
        (call_expression)
        (new_expression)
        (object)
        (array)
      ]) @definition.constant))
