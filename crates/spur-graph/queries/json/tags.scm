; Named object keys become modules. Scalars at document root become constants.
; Nested scalars become fields. Arrays and unnamed nodes are skipped.

(pair
  key: (string) @name
  value: (object)) @definition.module

(document
  (object
    (pair
      key: (string) @name
      value: [
        (string)
        (number)
        (true)
        (false)
        (null)
      ]) @definition.constant))

(pair
  value: (object
    (pair
      key: (string) @name
      value: [
        (string)
        (number)
        (true)
        (false)
        (null)
      ]) @definition.field))
