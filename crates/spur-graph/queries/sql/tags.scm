; SQL symbol tags. Follows the tree-sitter @definition.* / @name convention.

(create_table
  (object_reference
    name: (identifier) @name)) @definition.struct

(create_view
  (object_reference
    name: (identifier) @name)) @definition.type_alias

(create_function
  (object_reference
    name: (identifier) @name)) @definition.function

(create_index
  column: (identifier) @name) @definition.field

(create_type
  (object_reference
    name: (identifier) @name)
  (enum_elements)) @definition.enum

(create_schema
  .
  (keyword_create)
  .
  (keyword_schema)
  .
  (identifier) @name) @definition.module

(create_schema
  .
  (keyword_create)
  .
  (keyword_schema)
  .
  (keyword_if)
  .
  (keyword_not)
  .
  (keyword_exists)
  .
  (identifier) @name) @definition.module

(create_schema
  .
  (keyword_create)
  .
  (keyword_schema)
  .
  (keyword_authorization)
  .
  (identifier) @name) @definition.module

(column_definition
  name: (identifier) @name) @definition.field
