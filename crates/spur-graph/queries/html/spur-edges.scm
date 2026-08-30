; External script sources are imports.
((script_element
   (start_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @import.name @import.path
        (quoted_attribute_value
          (attribute_value) @import.name @import.path)]))) @import
 (#match? @_attribute "^[sS][rR][cC]$")
 (#not-match? @import.name "^[[:space:]]*[dD][aA][tT][aA]:"))

; Stylesheet links are imports. The rel value is an ASCII-case-insensitive
; space-separated token list. Tree-sitter query child patterns preserve sibling
; order, so both legal attribute orders need an explicit pattern.
((element
   [(start_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_rel_name
        [(attribute_value) @_rel_value
         (quoted_attribute_value
           (attribute_value) @_rel_value)])
      (attribute
        (attribute_name) @_href_name
        [(attribute_value) @import.name @import.path
         (quoted_attribute_value
           (attribute_value) @import.name @import.path)]))
    (self_closing_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_rel_name
        [(attribute_value) @_rel_value
         (quoted_attribute_value
           (attribute_value) @_rel_value)])
      (attribute
        (attribute_name) @_href_name
        [(attribute_value) @import.name @import.path
         (quoted_attribute_value
           (attribute_value) @import.name @import.path)]))]) @import
 (#match? @_tag "^[lL][iI][nN][kK]$")
 (#match? @_rel_name "^[rR][eE][lL]$")
 (#match? @_rel_value "(^|[[:space:]])[sS][tT][yY][lL][eE][sS][hH][eE][eE][tT]([[:space:]]|$)")
 (#match? @_href_name "^[hH][rR][eE][fF]$")
 (#not-match? @import.name "^[[:space:]]*[dD][aA][tT][aA]:"))

((element
   [(start_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_href_name
        [(attribute_value) @import.name @import.path
         (quoted_attribute_value
           (attribute_value) @import.name @import.path)])
      (attribute
        (attribute_name) @_rel_name
        [(attribute_value) @_rel_value
         (quoted_attribute_value
           (attribute_value) @_rel_value)]))
    (self_closing_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_href_name
        [(attribute_value) @import.name @import.path
         (quoted_attribute_value
           (attribute_value) @import.name @import.path)])
      (attribute
        (attribute_name) @_rel_name
        [(attribute_value) @_rel_value
         (quoted_attribute_value
           (attribute_value) @_rel_value)]))]) @import
 (#match? @_tag "^[lL][iI][nN][kK]$")
 (#match? @_href_name "^[hH][rR][eE][fF]$")
 (#match? @_rel_name "^[rR][eE][lL]$")
 (#match? @_rel_value "(^|[[:space:]])[sS][tT][yY][lL][eE][sS][hH][eE][eE][tT]([[:space:]]|$)")
 (#not-match? @import.name "^[[:space:]]*[dD][aA][tT][aA]:"))

; Anchor destinations are links. Exact attribute-name predicates reject
; similarly named attributes such as href-lang.
((element
   (start_tag
     (tag_name) @_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @link.name
        (quoted_attribute_value
          (attribute_value) @link.name)]))) @link
 (#match? @_tag "^[aA]$")
 (#match? @_attribute "^[hH][rR][eE][fF]$"))

; Media source attributes are links. Match both normal and self-closing tag
; forms while keeping the tag and attribute sets closed.
((element
   [(start_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_attribute
        [(attribute_value) @link.name
         (quoted_attribute_value
           (attribute_value) @link.name)]))
    (self_closing_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_attribute
        [(attribute_value) @link.name
         (quoted_attribute_value
           (attribute_value) @link.name)]))]) @link
 (#match? @_tag "^([iI][mM][gG]|[sS][oO][uU][rR][cC][eE]|[aA][uU][dD][iI][oO]|[vV][iI][dD][eE][oO])$")
 (#match? @_attribute "^[sS][rR][cC]$"))

; Video poster assets are links, separate from the src channel above.
((element
   [(start_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_attribute
        [(attribute_value) @link.name
         (quoted_attribute_value
           (attribute_value) @link.name)]))
    (self_closing_tag
      (tag_name) @_tag
      (attribute
        (attribute_name) @_attribute
        [(attribute_value) @link.name
         (quoted_attribute_value
           (attribute_value) @link.name)]))]) @link
 (#match? @_tag "^[vV][iI][dD][eE][oO]$")
 (#match? @_attribute "^[pP][oO][sS][tT][eE][rR]$"))
