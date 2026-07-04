; Shell symbol tags. Follows the tree-sitter @definition.* / @name convention.

(function_definition
  name: (word) @name) @definition.function

((command
  name: (command_name
    (word) @_alias_command)
  argument: (concatenation
    . (word) @name) @_alias_assignment) @definition.constant
 (#eq? @_alias_command "alias")
 (#match? @_alias_assignment "^[A-Za-z_][A-Za-z0-9_]*="))
