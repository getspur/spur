; SPUR notebook source facts for JavaScript and TypeScript cells.

; spur.put("name", value) - actual produce
((call_expression
  function: (member_expression object: (identifier) @_obj property: (property_identifier) @_method)
  arguments: (arguments . (string) @port.name)) @port.produce
 (#eq? @_obj "spur")
 (#eq? @_method "put"))

; spur.put(nameVar, value) - opaque dynamic produce marker
((call_expression
  function: (member_expression object: (identifier) @_obj_dynamic property: (property_identifier) @_method_dynamic)
  arguments: (arguments . (_) @_port.arg)) @port.produce
 (#eq? @_obj_dynamic "spur")
 (#eq? @_method_dynamic "put")
 (#not-match? @_port.arg "^[\"'`]"))

; spur.get("name") - actual consume
((call_expression
  function: (member_expression object: (identifier) @_obj2 property: (property_identifier) @_method2)
  arguments: (arguments . (string) @port.get.name)) @port.consume
 (#eq? @_obj2 "spur")
 (#eq? @_method2 "get"))

; bare table-function call: name(...) - candidate ds reference
(call_expression function: (identifier) @table.call) @table.ref
