; External script sources are imports.
((script_element
   (start_tag
     (attribute
       (attribute_name) @_attribute
       [(attribute_value) @import.name @import.path
        (quoted_attribute_value
          (attribute_value) @import.name @import.path)]))) @import
 (#eq? @_attribute "src"))

; Stylesheet links are imports. Tree-sitter query child patterns preserve
; sibling order, so both legal attribute orders need an explicit pattern.
((element
   (start_tag
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
          (attribute_value) @import.name @import.path)]))) @import
 (#eq? @_tag "link")
 (#eq? @_rel_name "rel")
 (#eq? @_rel_value "stylesheet")
 (#eq? @_href_name "href"))

((element
   (start_tag
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
          (attribute_value) @_rel_value)]))) @import
 (#eq? @_tag "link")
 (#eq? @_href_name "href")
 (#eq? @_rel_name "rel")
 (#eq? @_rel_value "stylesheet"))

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
 (#eq? @_tag "a")
 (#eq? @_attribute "href"))

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
 (#match? @_tag "^(img|source|audio|video)$")
 (#eq? @_attribute "src"))

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
 (#eq? @_tag "video")
 (#eq? @_attribute "poster"))
