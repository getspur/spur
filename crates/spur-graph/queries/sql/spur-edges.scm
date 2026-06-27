; SPUR-specific edge captures for SQL.

; ============ relation references ============

(relation
  (object_reference
    name: (identifier) @reference.name))

(from
  (object_reference
    name: (identifier) @reference.name))

(relation
  (invocation
    (object_reference
      name: (identifier) @reference.name)))

; ============ foreign key references ============

(column_definition
  (keyword_references)
  (object_reference
    name: (identifier) @reference.name))

(constraint
  (keyword_references)
  (object_reference
    name: (identifier) @reference.name))

; ============ write targets ============

(insert
  (object_reference
    name: (identifier) @reference.name))

; ============ DML consume-side sources ============

(insert
  (select)
  (from
    (relation
      [
        (object_reference
          name: (identifier) @reference.name)
        (invocation
          (object_reference
            name: (identifier) @reference.name))
        (subquery
          (from
            (relation
              [
                (object_reference
                  name: (identifier) @reference.name)
                (invocation
                  (object_reference
                    name: (identifier) @reference.name))
              ])))
      ])))

(update
  (from
    (relation
      [
        (object_reference
          name: (identifier) @reference.name)
        (invocation
          (object_reference
            name: (identifier) @reference.name))
        (subquery
          (from
            (relation
              [
                (object_reference
                  name: (identifier) @reference.name)
                (invocation
                  (object_reference
                    name: (identifier) @reference.name))
              ])))
      ])))

(update
  (assignment
    (subquery
      (from
        (relation
          [
            (object_reference
              name: (identifier) @reference.name)
            (invocation
              (object_reference
                name: (identifier) @reference.name))
          ])))))

(update
  (where
    (binary_expression
      (subquery
        (from
          (relation
            [
              (object_reference
                name: (identifier) @reference.name)
              (invocation
                (object_reference
                  name: (identifier) @reference.name))
            ]))))))

(statement
  (delete)
  (from
    (where
      (binary_expression
        (subquery
          (from
            (relation
              [
                (object_reference
                  name: (identifier) @reference.name)
                (invocation
                  (object_reference
                    name: (identifier) @reference.name))
              ])))))))

(statement
  (keyword_merge)
  (keyword_using)
  [
    (object_reference
      name: (identifier) @reference.name)
    (subquery
      (from
        (relation
          [
            (object_reference
              name: (identifier) @reference.name)
            (invocation
              (object_reference
                name: (identifier) @reference.name))
          ])))
  ])

; ============ call sites ============

(invocation
  (object_reference
    name: (identifier) @call.name)) @call
