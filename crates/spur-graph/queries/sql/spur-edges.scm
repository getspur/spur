; SPUR-specific edge captures for SQL.

; ============ relation references ============

(relation
  (object_reference
    name: (identifier) @reference.name))

(from
  (object_reference
    name: (identifier) @reference.name))

; ============ write targets ============

(insert
  (object_reference
    name: (identifier) @reference.name))

; ============ call sites ============

(invocation
  (object_reference
    name: (identifier) @call.name)) @call
