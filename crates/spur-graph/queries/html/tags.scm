; ID-bearing normal elements are graph sections. Capture the whole element so
; source spans and containment reflect the semantic region, while @name holds
; only the quote-free attribute value.
((element
   (start_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @name
        (quoted_attribute_value
          (attribute_value) @name)]))) @definition.section
 (#match? @_attribute "^[iI][dD]$"))

; Self-closing elements use a distinct tag node in tree-sitter-html.
((element
   (self_closing_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @name
        (quoted_attribute_value
          (attribute_value) @name)]))) @definition.section
 (#match? @_attribute "^[iI][dD]$"))

; Script and style elements are distinct grammar nodes whose specialized start
; tags are aliased to start_tag in the public syntax tree.
((script_element
   (start_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @name
        (quoted_attribute_value
          (attribute_value) @name)]))) @definition.section
 (#match? @_attribute "^[iI][dD]$"))

((style_element
   (start_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @name
        (quoted_attribute_value
          (attribute_value) @name)]))) @definition.section
 (#match? @_attribute "^[iI][dD]$"))
