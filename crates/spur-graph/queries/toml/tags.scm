; Named tables and inline tables become modules. Root scalars are constants.
; Nested table/inline-table scalars are fields. Array-of-tables wrappers are skipped.

(table
  (bare_key) @name) @definition.module

(table
  (dotted_key) @name) @definition.module

(pair
  (bare_key) @name
  (inline_table)) @definition.module

(document
  (pair
    (bare_key) @name
    [
      (string)
      (integer)
      (float)
      (boolean)
      (local_date)
      (local_date_time)
      (local_time)
      (offset_date_time)
    ]) @definition.constant)

(table
  (pair
    (bare_key) @name
    [
      (string)
      (integer)
      (float)
      (boolean)
      (local_date)
      (local_date_time)
      (local_time)
      (offset_date_time)
    ]) @definition.field)

(inline_table
  (pair
    (bare_key) @name
    [
      (string)
      (integer)
      (float)
      (boolean)
      (local_date)
      (local_date_time)
      (local_time)
      (offset_date_time)
    ]) @definition.field)

(table_array_element
  (pair
    (bare_key) @name
    [
      (string)
      (integer)
      (float)
      (boolean)
      (local_date)
      (local_date_time)
      (local_time)
      (offset_date_time)
    ]) @definition.field)
