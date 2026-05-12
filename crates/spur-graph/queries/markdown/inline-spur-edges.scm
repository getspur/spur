(inline_link
  (link_destination) @import.name) @import

(full_reference_link
  (link_label) @import.name) @import

; For collapsed/shortcut references, tree-sitter-markdown exposes the label as
; link_text (syntax: [label][] / [label]).
(collapsed_reference_link
  (link_text) @import.name) @import

(shortcut_link
  (link_text) @import.name) @import
