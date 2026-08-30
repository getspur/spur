; CSS semantic definitions.

(rule_set
  (selectors) @name) @definition.section

(keyframes_statement
  (keyframes_name) @name) @definition.function

; The grammar aliases a final declaration without `;` back to `declaration`,
; so this pattern covers both declaration forms.
((declaration
   (property_name) @name) @definition.constant
 (#match? @name "^--"))
