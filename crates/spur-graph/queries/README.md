# Tree-sitter Query Contract

This directory contains the query sources used by `crates/spur-graph` to turn
tree-sitter captures into SPUR graph nodes and edges. Each supported language is
wired through `src/extract/languages.rs` with a registry entry, a
`LanguageConfig`, and a `definition_kind_map`.

## Definition Capture Vocabulary

Definition queries follow the tree-sitter tags convention:

- use `@definition.<kind>` on the node that should become a symbol
- provide an inner `@name` capture for the display/FQN label
- add every `@definition.<kind>` capture to the language's
  `definition_kind_map`

The shared capture vocabulary is:

| Capture | NodeKind |
|---|---|
| `@definition.module` | `NodeKind::Module` |
| `@definition.function` | `NodeKind::Function` |
| `@definition.method` | `NodeKind::Method` |
| `@definition.class` | `NodeKind::Class` |
| `@definition.interface` | `NodeKind::Interface` |
| `@definition.struct` | `NodeKind::Struct` |
| `@definition.enum` | `NodeKind::Enum` |
| `@definition.impl` | `NodeKind::Impl` |
| `@definition.trait` | `NodeKind::Trait` |
| `@definition.type_alias` | `NodeKind::TypeAlias` |
| `@definition.macro` | `NodeKind::Macro` |
| `@definition.field` | `NodeKind::Field` |
| `@definition.constant` | `NodeKind::Constant` |
| `@definition.section` | `NodeKind::Section` |
| `@definition.enum_variant` | `NodeKind::EnumVariant` |

`@definition.constant` is part of the contract vocabulary but is not yet
captured by any checked-in language row. `@definition.enum_variant` is captured
for Rust today; other languages with enum members (TypeScript, C++) remain
`TODO`.

## Coverage Matrix

Legend:

- `Y`: captured today and expected by the automated gate
- `-`: the language family legitimately lacks this construct or SPUR does not
  model it for that family
- `TODO`: known coverage gap; do not add a new language with an unreviewed gap

| Language | module | function | method | class | interface | struct | enum | impl | trait | type_alias | macro | field | section | constant | enum_variant |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Rust | Y | Y | Y | - | - | Y | Y | Y | Y | Y | Y | TODO | - | TODO | Y |
| Python | - | Y | - | Y | - | - | - | - | - | - | - | - | - | TODO | - |
| TypeScript | Y | Y | Y | Y | Y | - | Y | - | - | Y | - | TODO | - | TODO | TODO |
| Tsx | Y | Y | Y | Y | Y | - | Y | - | - | Y | - | TODO | - | TODO | TODO |
| Cpp | Y | Y | Y | Y | - | Y | Y | - | - | Y | Y | Y | - | TODO | TODO |
| Markdown | - | - | - | - | - | - | - | - | - | - | - | - | Y | - | - |

Notes:

- Python methods are captured as `@definition.function` and reclassified by
  the adapter when the function is nested inside a class.
- C++ is the only current language capturing `@definition.field`.
- No current language captures `@definition.constant`. Canonical Python
  `tags.scm` patterns usually include module-level constants, but SPUR does not
  yet.
- Rust captures enum members as `@definition.enum_variant` (mapped to
  `NodeKind::EnumVariant`); TypeScript enum members and C++ `enumerator`s
  remain `TODO`.
- Rust `union_item` is captured as `@definition.struct` (folded into the
  `struct` column above), matching how C++ models unions.
- `python/symbols.scm` currently duplicates `python/tags.scm`; snapshot
  extraction uses the tags query when there is no separate symbols policy.

## Reference Capture Divergence

SPUR deliberately diverges from canonical tree-sitter `tags.scm` reference
captures. Language `tags.scm` files define symbols with `@definition.*`, but
SPUR does not rely on canonical `@reference.*` captures for graph edges.

Instead, each language provides `spur-edges.scm` patterns for richer edge
extraction, including call edges, higher-order-function references, macro-body
calls, JSX render edges, and Markdown links. Contributors should add or update
edge captures in `spur-edges.scm` rather than expecting `@reference.*` to drive
SPUR graph relations.

## Adding A New Language Family

Before adding a new `Language` variant or registry row, complete this checklist.
These are the human-readable version of the in-crate gate test in
`src/extract/languages.rs`.

- [ ] Add one `language_registry()` entry with at least one non-empty file
      extension.
- [ ] Confirm none of the new extensions collide with extensions owned by an
      existing language family.
- [ ] Add a `LanguageConfig` whose `queries` slice contains a non-empty
      `"tags"` query source.
- [ ] Ensure every configured query compiles against the configured
      tree-sitter grammar. Inline Markdown-style queries must compile against
      their configured `inline_language`.
- [ ] Add a non-empty `definition_kind_map`.
- [ ] Ensure every `@definition.*` capture in every configured query has a
      matching `definition_kind_map` key.
- [ ] Ensure every `definition_kind_map` key appears as a capture in at least
      one configured query source.
- [ ] Ensure every `NodeKind` used by the map has an explicit `symbol_kind()`
      result and does not fall back to `"symbol"`.
- [ ] Add the language's current expected `@definition.<kind>` set to the
      contract test's coverage rows.
- [ ] Update the coverage matrix in this README with `Y`, `-`, or `TODO` for
      every shared definition kind.
