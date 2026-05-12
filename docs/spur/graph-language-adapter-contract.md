# SPUR Graph Language Adapter Contract

Phase 2B language adapters are query-driven. The shared tree-sitter emitter consumes captures with these conventions:

- `tags.scm` emits wrapper captures named `@definition.<kind>` and an inner `@name` capture inside each wrapper. `LanguageConfig::definition_kind_map` maps each wrapper capture name to a `NodeKind`.
- `spur-edges.scm` emits edge wrapper captures named `@import` and `@call`. Each wrapper contains one or more inner `@import.name` or `@call.name` captures.
- The emitter associates inner captures to wrappers by byte-range containment. The inner capture text is the node label or pending edge target label.
- Language-specific definition disambiguation belongs in `LanguageConfig::is_method`. Rust uses this to preserve impl-body functions as `NodeKind::Method`; other languages can leave it as `None`.
- If a definition wrapper has no inner `@name`, the shared emitter skips that definition. Add query coverage instead of adding language-specific field lookups.

Adding a new language should require the grammar dependency, `queries/<lang>/{tags.scm, spur-edges.scm}`, and a small `<lang>_config()` that supplies the language, query paths, definition map, and optional method classifier.
