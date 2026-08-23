; Named mappings become modules. Top-level scalars are constants.
; Nested scalars are fields. Sequences (arrays) and unnamed nodes are skipped.

(block_mapping_pair
  key: (flow_node
    [
      (plain_scalar (string_scalar) @name)
      (double_quote_scalar) @name
      (single_quote_scalar) @name
    ])
  value: (block_node (block_mapping))) @definition.module

(stream
  (document
    (block_node
      (block_mapping
        (block_mapping_pair
          key: (flow_node
            [
              (plain_scalar (string_scalar) @name)
              (double_quote_scalar) @name
              (single_quote_scalar) @name
            ])
          value: (flow_node)) @definition.constant))))

(block_mapping_pair
  value: (block_node
    (block_mapping
      (block_mapping_pair
        key: (flow_node
          [
            (plain_scalar (string_scalar) @name)
            (double_quote_scalar) @name
            (single_quote_scalar) @name
          ])
        value: (flow_node)) @definition.field)))
