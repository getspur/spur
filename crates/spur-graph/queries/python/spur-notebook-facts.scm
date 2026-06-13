; SPUR notebook source facts for Python cells.

; spur.put("name", value) - actual produce
((call
  function: (attribute object: (identifier) @_obj attribute: (identifier) @_method)
  arguments: (argument_list . (string) @port.name)) @port.produce
 (#eq? @_obj "spur")
 (#eq? @_method "put"))

; spur.put(name_var, value) - opaque dynamic produce marker
((call
  function: (attribute object: (identifier) @_obj_dynamic attribute: (identifier) @_method_dynamic)
  arguments: (argument_list . (_) @_port.arg)) @port.produce
 (#eq? @_obj_dynamic "spur")
 (#eq? @_method_dynamic "put")
 (#not-match? @_port.arg "^[\"']"))

; spur.get("name") - actual consume
((call
  function: (attribute object: (identifier) @_obj2 attribute: (identifier) @_method2)
  arguments: (argument_list . (string) @port.get.name)) @port.consume
 (#eq? @_obj2 "spur")
 (#eq? @_method2 "get"))

; bare table-function call: name(...) - candidate ds reference
(call function: (identifier) @table.call) @table.ref
