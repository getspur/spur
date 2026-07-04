# Tree-sitter Query Contract

This directory contains the query sources used by `crates/spur-graph` to turn
tree-sitter captures into SPUR graph nodes and edges. Each supported language is
wired through `src/extract/languages.rs` with a registry entry, a
`LanguageConfig`, and a `definition_kind_map`.

For ontology expansion policy, see the focused maturity roadmap:
[`docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md`](../../../docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md).
It defines when syntax captures should remain labels/metadata versus being
promoted to first-class `NodeKind` or `RelationKind` variants.

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
| `@definition.resource` | `NodeKind::Resource` |

`@definition.constant` is captured for Rust, Python, TypeScript/TSX/JavaScript
top-level non-function `const` bindings, C file-scope `const` variables, and
C++ namespace/file-scope `const` and `constexpr` variables, and Go
package-level `const` and `var` bindings, Shell aliases, and SQL indexes.
`@definition.enum_variant` is captured for Rust, TypeScript/TSX/JavaScript, C,
and C++ enum members.
`@definition.resource` is captured for Hcl/Terraform `resource` and `provider`
blocks; the family's `@definition.data` and `@definition.module` captures fold
into the same `NodeKind::Resource` column, and `@definition.variable` /
`@definition.output` / `@definition.local` fold into `constant`.

## Coverage Matrix

Legend:

- `Y`: captured today and expected by the automated gate
- `-`: the language family legitimately lacks this construct or SPUR does not
  model it for that family
- `TODO`: known coverage gap; do not add a new language with an unreviewed gap

| Language | module | function | method | class | interface | struct | enum | impl | trait | type_alias | macro | field | section | constant | enum_variant | resource |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Rust | Y | Y | Y | - | - | Y | Y | Y | Y | Y | Y | Y | - | Y | Y | - |
| Python | - | Y | - | Y | - | - | - | - | - | - | - | - | - | Y | - | - |
| TypeScript | Y | Y | Y | Y | Y | - | Y | - | - | Y | - | Y | - | Y | Y | - |
| Tsx | Y | Y | Y | Y | Y | - | Y | - | - | Y | - | Y | - | Y | Y | - |
| JavaScript | Y | Y | Y | Y | Y | - | Y | - | - | Y | - | Y | - | Y | Y | - |
| C | - | Y | - | - | - | Y | Y | - | - | Y | Y | Y | - | Y | Y | - |
| Cpp | Y | Y | Y | Y | - | Y | Y | - | - | Y | Y | Y | - | Y | Y | - |
| Go | Y | Y | Y | - | Y | Y | - | - | - | Y | - | Y | - | Y | - | - |
| Hcl | - | - | - | - | - | - | - | - | - | - | - | - | - | Y | - | Y |
| Terraform | - | - | - | - | - | - | - | - | - | - | - | - | - | Y | - | Y |
| Lua | - | Y | Y | - | - | - | - | - | - | - | - | - | - | - | - | - |
| Shell | - | Y | - | - | - | - | - | - | - | - | - | - | - | Y | - | - |
| Sql | Y | Y | - | - | - | Y | Y | - | - | Y | - | Y | - | Y | - | - |
| Markdown | - | - | - | - | - | - | - | - | - | - | - | - | Y | - | - | - |
| JupyterNotebook | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |

Notes:

- Python methods are captured as `@definition.function` and reclassified by
  the adapter when the function is nested inside a class.
- Rust captures named struct fields as `@definition.field`; tuple-struct
  positional fields are intentionally skipped. TypeScript/TSX/JavaScript
  configs capture class fields with no initializer or a simple non-function
  initializer. Function-valued class fields are captured as
  `@definition.function` instead of also being emitted as plain fields. C++
  captures simple class data members.
- Rust captures `const_item` and `static_item` as `@definition.constant`.
  Python follows the canonical module-level assignment constant pattern.
  TypeScript/TSX/JavaScript configs capture top-level and exported `const`
  bindings with non-function initializer forms; direct arrow/function
  initializer forms are emitted as `@definition.function` only. C captures
  file-scope `const` variables. C++ captures namespace/file-scope `const` and
  `constexpr` variables; const class members remain fields, and const locals,
  parameters, and function return types are intentionally skipped.
- Rust captures enum members as `@definition.enum_variant` (mapped to
  `NodeKind::EnumVariant`); TypeScript/TSX/JavaScript configs capture enum
  members and C++ `enumerator`s do the same.
- Rust `union_item` is captured as `@definition.struct` (folded into the
  `struct` column above), matching how C++ models unions.
- `python/symbols.scm` used to duplicate `python/tags.scm`. Snapshot extraction
  now reuses Python `tags.scm`, matching Rust/TypeScript/C++ and avoiding a
  stale duplicate symbol policy.
- Go methods are grammatical (`method_declaration` for receiver methods and
  `method_elem` for interface methods), so no `is_method` heuristic is used.
  `type_spec` bodies dispatch to `struct`/`interface`, with every other
  `type_spec` shape and `type_alias` folded into `type_alias`. Multi-name
  `const_spec`, `var_spec`, and struct `field_declaration` specs capture each
  identifier as its own definition node so `const A, B = 1, 2` and
  `var A, B = 1, 2` fan out into one constant per name. Only top-level `const`
  and `var` declarations are captured (function-local declarations are
  skipped). Receiver methods use the receiver type as their logical scope, so
  `func (p Point) Scale()` and `func (p *Path) Scale()` remain bare `Scale`
  labels but become `Point::Scale` / `Path::Scale` identities. The Go
  `package_clause` spans only the clause itself, so other top-level symbols are
  not nested under the module node and keep bare-name FQNs, matching the
  shell/sql convention.
- Hcl (.hcl) and Terraform (.tf) share the tree-sitter-hcl grammar and the
  same query set. Symbols carry canonical Terraform address labels:
  `resource`/`data`/`module` blocks become `NodeKind::Resource` nodes labeled
  `<type>.<name>` / `data.<type>.<name>` / `module.<name>`, provider blocks
  become `NodeKind::Resource` nodes labeled `<provider>` or
  `<provider>.<alias>`, and `variable`/`output` blocks plus each `locals`
  attribute become
  `NodeKind::Constant` nodes labeled `var.<name>` / `output.<name>` /
  `local.<name>`. Provider references such as `provider = aws.west` resolve to
  the aliased provider block when that block is present in the same module
  scope.
  `.tf.json` files are JSON syntax and are out of scope (the extension
  matcher sees `json`); Terragrunt-flavored `.hcl` parses as generic HCL with
  no dedicated `dependency`/`include` modeling.
- Shell captures function definitions and `alias name=value` declarations;
  aliases fold into `NodeKind::Constant`.
- Sql captures `CREATE TRIGGER` as `NodeKind::Function` and `CREATE INDEX` as
  `NodeKind::Constant`. `tree-sitter-sequel` 0.3 exposes no
  `create_procedure` node, so `CREATE PROCEDURE` remains a grammar-blocked
  TODO.

## Relation Coverage Matrix

Predicate realization per language family (`Y` realized · `—` not realizable · `TODO` gap).
This table is the relation-level analogue of the Definition Coverage Matrix and the
seed of the Tier-0 ontology realization contract
(`docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`).

| Predicate | Rust | Python | TypeScript | Tsx | JavaScript | C | Cpp | Go | Hcl | Terraform | Lua | Shell | Sql | Markdown | JupyterNotebook |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| imports | Y | Y | Y | Y | Y | Y | Y | Y | — | — | Y | Y | — | Y(links) | — |
| calls | Y | Y | Y | Y | Y | Y | Y | Y | — | — | Y | Y | Y | — | — |
| constructs | Y | Y | Y | Y | Y | Y | Y | Y | — | — | — | — | Y | — | — |
| contains | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| produces | — | — | — | — | — | — | — | — | — | — | — | — | — | — | Y |
| consumes | — | — | — | — | — | — | — | — | — | — | — | — | — | — | Y |
| binds | — | — | — | — | — | — | — | — | — | — | — | — | — | — | Y |
| emits | — | — | — | — | — | — | — | — | — | — | — | — | — | — | Y |
| defines | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | Y | — | — |
| references (HOF/notebook/address) | Y | Y | Y | Y | Y | — | Y | — | Y | Y | — | — | Y | — | Y |
| links | — | — | — | — | — | — | — | — | — | — | — | — | — | Y | — |
| implements | Y | Y | Y | Y | Y | — | — | — | — | — | — | — | — | — | — |
| extends | Y | Y | Y | Y | Y | — | Y | — | — | — | — | — | — | — | — |

Python `implements` is realized after resolution: `@extends` captures targeting
local classes declared with `Protocol`, `ABC`, or `abc.ABC` bases are
reclassified to `implements`.

TypeScript/Tsx/JavaScript `imports` includes both direct import declarations
and re-export statements (`export { ... } from "..."`, `export * from "..."`),
both mapped to `RelationKind::Imports` through the same resolver path.

Go `imports` edges are emitted per `import_spec`. The quote-free path content
(`interpreted_string_literal_content` / `raw_string_literal_content`) is
captured as `@import.path` and doubles as `@import.name` for plain, dot, and
blank imports; aliased imports use the alias `package_identifier` as the edge
name instead. Bare single-segment paths such as `fmt` are kept via
`preserve_bare_import_path`. Go `constructs` is realized after resolution:
composite literals of named types (`type_identifier`, `qualified_type`,
`generic_type`) emit Calls edges that reclassify to Constructs when the target
resolves to a struct; slice, array, and map literal shapes emit nothing.

HOF references are realized in Rust, Python, C++, and TypeScript/Tsx/JavaScript
via closed HOF-method allowlists in the respective `spur-edges.scm` files.
TypeScript/Tsx/JavaScript currently capture bare identifier callbacks passed as
the first argument to recognized array/iterable and Promise-style methods.

Markdown `links` covers inline links plus full, collapsed, and shortcut
reference-style links. Reference definitions map labels to normalized
destinations; missing labels remain unresolved link edges labeled by the
normalized reference label.

Hcl/Terraform `references` are address references
(`GraphEdgeKind::ReferencesAddress`): each `variable_expr` + `get_attr` chain
(bare or inside `${...}` template interpolation, with `index` nodes skipped)
is truncated to its canonical Terraform address — `var.<name>`,
`local.<name>`, `module.<name>` (output tails trimmed), `data.<type>.<name>`,
or `<type>.<name>`. The reserved builtin roots `count`, `each`, `self`,
`path`, `terraform`, and `provider` and single bare identifiers emit no
edges. Resolution is module-directory-first (`address_module_scope`), then
workspace-singleton (`address_singleton`), restricted to `Resource`/
`Constant` targets in the hcl language family; anything else stays
unresolved evidence. No call channel exists — all Terraform functions are
builtins. Recall ceiling: address references resolve iff the target address
is defined in the same module directory (or is a workspace singleton), which
Terraform semantics make a tight bound; the expected residue on idiomatic
multi-module repos (~5–15%) is for-expression loop-var attribute access and
exotic splats — all left unresolved, never wrongly bound.

### Notebook Semantic Facts

Jupyter notebook extraction reserves semantic fact relations for slice-3 data
flow:

- `produces`: a cell writes a named `port://...` value.
- `consumes`: a cell reads a named `port://...` value.
- `binds`: a frontend cell binds UI state to a port.
- `emits`: a frontend cell emits UI events or values to a port.
- `references`: a cell references a datasource such as `ds://...`.

Python and TypeScript notebook-facts queries are present, registered in their
language configs, and included in `MANIFEST_QUERY_BYTES`. They detect actual
`spur.put` produces, `spur.get` consumes, dynamic/opaque `spur.put` produce
markers, and bare table-function references from cell source. Metadata-based
`binds` and `emits` are emitted from notebook metadata, not from those
source-level query files.

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
