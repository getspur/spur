; SPUR-specific edge captures for shell scripts.

; ============ imports ============

((command
   name: (command_name
     (word) @source.command)
   argument: [
     (word) @import.name
     (string) @import.name
     (raw_string) @import.name
   ]) @import
 (#match? @source.command "^(source|\\.)$"))

; ============ call sites ============

(command
  name: (command_name
    (word) @call.name)) @call
