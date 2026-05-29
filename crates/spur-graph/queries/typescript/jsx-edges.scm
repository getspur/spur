; TSX-only graph edges. These reference JSX grammar nodes that exist solely
; in tree-sitter-typescript's TSX grammar (the plain TypeScript grammar has no
; JSX), so this query is wired in by `tsx_config()` only.
;
; JSX render sites: `<Component />` and `<Component>…</Component>` become Calls
; edges so React component trees are visible to code_callers/callees. This is
; intentionally beyond the upstream tags.scm surface, which never tags JSX
; (JSX appears only in highlights-jsx.scm upstream).
;
; The `jsx_call` capture (not `call`) is used because the edge emitter applies
; the React naming rule: only uppercase-initial tags are component references;
; lowercase tags (`div`, `span`, …) are intrinsic host elements and must not
; produce call edges. SPUR's matcher does not evaluate `#match?` predicates, so
; the filter lives in `emit_edges`.
(jsx_opening_element
  name: (identifier) @jsx_call.name) @jsx_call

(jsx_self_closing_element
  name: (identifier) @jsx_call.name) @jsx_call
