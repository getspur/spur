; HCL/Terraform symbol tags. The adapter assembles Terraform addresses from the label captures.

((block
  . (identifier) @_kw
  . (string_lit) @resource.type
  . (string_lit) @resource.name
  . (block_start)) @definition.resource
 (#eq? @_kw "resource"))

((block
  . (identifier) @_kw
  . (string_lit) @resource.type
  . (string_lit) @resource.name
  . (block_start)) @definition.data
 (#eq? @_kw "data"))

((block
  . (identifier) @_kw
  . (string_lit) @resource.name
  . (block_start)) @definition.module
 (#eq? @_kw "module"))

((block
  . (identifier) @_kw
  . (string_lit) @provider.type
  . (block_start)) @definition.resource
 (#eq? @_kw "provider"))

((block
  . (identifier) @_kw
  . (string_lit) @resource.name
  . (block_start)) @definition.variable
 (#eq? @_kw "variable"))

((block
  . (identifier) @_kw
  . (string_lit) @resource.name
  . (block_start)) @definition.output
 (#eq? @_kw "output"))

((block
  . (identifier) @_kw
  . (block_start)
  (body
    (attribute
      . (identifier) @name) @definition.local))
 (#eq? @_kw "locals"))
