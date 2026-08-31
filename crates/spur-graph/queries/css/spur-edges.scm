; CSS imports written as a quoted string.
((import_statement
   (string_value
     (string_content) @import.name @import.path)) @import
 (#not-match? @import.name "^[[:space:]]*[dD][aA][tT][aA]:"))

; CSS imports written as url(...), quoted or simple-unquoted.
((import_statement
   (call_expression
     (function_name) @_import_function
     (arguments
       . [(plain_value) @import.name @import.path
          (string_value
            (string_content) @import.name @import.path)] .))) @import
 (#match? @_import_function "^[uU][rR][lL]$")
 (#not-match? @import.name "^[[:space:]]*[dD][aA][tT][aA]:"))

; Asset URLs. Every call has an immediate named parent; rejecting an
; import-statement parent prevents @import url(...) from also becoming Links.
; Argument anchors require one target node and avoid partial data-URI captures.
((_
   (call_expression
     (function_name) @_function
     (arguments
       . [(plain_value) @link.name
          (color_value) @link.name
          (string_value
            (string_content) @link.name)] .)) @link) @_url_parent
 (#match? @_function "^[uU][rR][lL]$")
 (#not-match? @_url_parent "^@[iI][mM][pP][oO][rR][tT]([[:space:]]|$)")
 (#not-match? @link.name "^[[:space:]]*[dD][aA][tT][aA]:"))
