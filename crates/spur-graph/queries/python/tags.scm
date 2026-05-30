; Python symbol tags with standard @definition.* captures and inner @name labels.

(function_definition
  name: (identifier) @name) @definition.function

(class_definition
  name: (identifier) @name) @definition.class

(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.constant))
