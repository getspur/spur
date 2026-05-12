# SPUR Graph Language Adapter Contract

Phase 2B language adapters are query-driven. The shared tree-sitter emitter consumes captures with these conventions:

- `tags.scm` emits wrapper captures named `@definition.<kind>` and an inner `@name` capture inside each wrapper. `LanguageConfig::definition_kind_map` maps each wrapper capture name to a `NodeKind`.
- `spur-edges.scm` emits block-grammar edge wrapper captures named `@import` and `@call`. Each wrapper contains one or more inner `@import.name` or `@call.name` captures.
- If a language uses a separate inline grammar, it can additionally provide `inline-spur-edges.scm` for inline-grammar edge captures. The extractor compiles `spur-edges` against the block language and `inline-spur-edges` against `inline_language`; there is no XOR between them.
- Languages can override relation semantics per capture wrapper via `LanguageConfig::relation_kind_map`. For example, markdown maps `@import` captures to `RelationKind::Links` while Rust/Python/TypeScript keep `@import -> RelationKind::Imports`.
- The emitter associates inner captures to wrappers by byte-range containment. The inner capture text is the node label or pending edge target label.
- Language-specific definition disambiguation belongs in `LanguageConfig::is_method`. Rust uses this to preserve impl-body functions as `NodeKind::Method`; other languages can leave it as `None`.
- If a definition wrapper has no inner `@name`, the shared emitter skips that definition. Add query coverage instead of adding language-specific field lookups.

Adding a new language should require the grammar dependency, `queries/<lang>/{tags.scm, spur-edges.scm}` (plus `inline-spur-edges.scm` when needed), a small `<lang>_config()` that supplies the language, query paths, definition map, and optional method classifier, and one `LanguageDescriptor` row in `language_registry()` (matcher, factory, label, extensions). The registry is the single source of truth for dispatch, supported extension discovery, and CLI per-language file counts.

If a grammar's tree shape does not encode semantic hierarchy directly, use a language-specific post-process module called from `extract_files` for that language group. Canonical pattern: markdown headings are siblings in block grammar, so the markdown adapter builds `RelationKind::Contains` hierarchy by sorting section definitions and applying a heading-level stack (`h1 -> h2 -> h3`). Keep this behavior out of the shared generic emitter.
