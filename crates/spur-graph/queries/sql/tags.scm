; SQL symbol tags. Follows the tree-sitter @definition.* / @name convention.

(create_table
  (object_reference
    name: (identifier) @name)) @definition.struct

(create_view
  (object_reference
    name: (identifier) @name)) @definition.type_alias

(create_materialized_view
  (object_reference
    name: (identifier) @name)) @definition.type_alias

(create_function
  (object_reference
    name: (identifier) @name)) @definition.function

(create_trigger
  (keyword_trigger)
  (object_reference
    name: (identifier) @name)
  [
    (keyword_before)
    (keyword_after)
    (keyword_instead)
  ]) @definition.function

(create_index
  (keyword_index)
  column: (identifier) @name
  (keyword_on)) @definition.constant

(create_type
  (object_reference
    name: (identifier) @name)
  (keyword_as)
  (keyword_enum)
  (enum_elements)) @definition.enum

(create_type
  (object_reference
    name: (identifier) @name)
  (keyword_as)
  (column_definitions)) @definition.struct

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
  name: (identifier) @name
  (#not-match? @name "^[Pp][Aa][Rr][Tt][Ii][Tt][Ii][Oo][Nn][Ee][Dd]$")) @definition.field
