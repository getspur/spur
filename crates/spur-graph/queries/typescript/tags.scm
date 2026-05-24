; Follows the standard tree-sitter tags.scm convention of @definition.*
; captures for code navigation symbols, with inner @name captures for labels.

(class_declaration
  name: (type_identifier) @name) @definition.class

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(interface_declaration
  body: (interface_body
    (method_signature
      name: (property_identifier) @name) @definition.method))

(enum_declaration
  name: (identifier) @name) @definition.enum

(function_declaration
  name: (identifier) @name) @definition.function

(variable_declarator
  name: (identifier) @name
  value: (arrow_function)) @definition.function

(method_definition
  name: (property_identifier) @name) @definition.method

(type_alias_declaration
  name: (type_identifier) @name) @definition.type_alias
