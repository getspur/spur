; Go symbol tags. Follows the tree-sitter @definition.* / @name convention.

(package_clause
  (package_identifier) @name) @definition.module

(function_declaration
  name: (identifier) @name) @definition.function

(method_declaration
  name: (field_identifier) @name) @definition.method

(type_spec
  name: (type_identifier) @name
  type: (struct_type)) @definition.struct

(type_spec
  name: (type_identifier) @name
  type: (interface_type)) @definition.interface

(type_spec
  name: (type_identifier) @name) @definition.type_alias

(type_alias
  name: (type_identifier) @name) @definition.type_alias

(source_file
  (const_declaration
    (const_spec
      (identifier) @name @definition.constant)))

(source_file
  (var_declaration
    (var_spec
      (identifier) @name @definition.constant)))

(field_declaration
  (field_identifier) @name @definition.field)

(method_elem
  name: (field_identifier) @name) @definition.method
