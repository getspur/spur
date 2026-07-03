# Phase 1b Design Spec: HCL + Terraform Support in `spur-graph`

Status: design spec (pre-implementation)
Crate: `crates/spur-graph`
Grammar: `tree-sitter-hcl` 1.1.0 (one grammar covers `.tf` and `.hcl`)
Companions:
- `docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md`
- `crates/spur-graph/queries/README.md`

Expert review context this spec builds on (converged conclusions, not re-litigated
here): `NodeKind::Resource` is promoted narrowly for Terraform resource/data/module
blocks and is NOT folded into `Struct`; `NodeKind::Variable` is dropped in favor of
`NodeKind::Constant` with address labels (`var.x`, `local.x`, `output.x`); Go
imports stay edges and are out of scope.

House style note: the eventual implementation adds **no code comments** except
where a constraint cannot be expressed by the code itself, matching the
surrounding `spur-graph` idiom.

---

## 1. Reference Channel (the crux)

### 1.1 Why the naive design fails — verified against source

The obvious move is to reuse the existing `reference.name` capture channel. It
fails three ways, all confirmed in source:

**(1) The ReferencesHof guard rejects every bind.** The `reference.name` arm of
`emit_edges` unconditionally stamps `edge_kind: Some(GraphEdgeKind::ReferencesHof)`
(`crates/spur-graph/src/extract/languages.rs:1116-1131`, stamp at
`languages.rs:1126`). At resolution, the `References` arm of
`resolve_pending_edges` (`crates/spur-graph/src/extract/tree_sitter.rs:691-716`)
binds a label singleton only when the target kind is `Function | Method`
(`tree_sitter.rs:695-698`). Any other bind — and even the emission of an
*unresolved* edge — is gated on `reference_fallbacks_allowed`
(`tree_sitter.rs:711-716`), which returns false for `ReferencesHof` edges unless
the source file ends in `.sql` (`tree_sitter.rs:810-815`). HCL reference targets
are `Resource`/`Constant` nodes, never `Function`/`Method`, so every HCL edge on
this channel would be **silently dropped** — not even preserved as unresolved
evidence.

**(2) A plain References channel phantom-binds with no guard.** If the HCL
channel instead emitted `edge_kind: None`, `reference_fallbacks_allowed` returns
true and resolution falls into the unguarded arm at `tree_sitter.rs:711-712`:
`singleton_symbols_by_label` bind with **no target-kind check and no language
check**. A Markdown section, SQL table, or Python constant whose label happens to
equal the reference text would receive the edge. (This exposure exists today for
SQL via the `.sql` escape hatch at `tree_sitter.rs:812-814`; Phase 1b must not
widen it.)

**(3) Workspace-global label singletons collapse in multi-module repos.**
`singleton_symbols_by_label` is built from the workspace-global `symbol_index`
(`tree_sitter.rs:578-590`; labels inserted at `tree_sitter.rs:425-431`). Two
Terraform modules both defining `aws_instance.web` make the label ambiguous
(`ambiguous_symbols_by_label`, `tree_sitter.rs:586-589`), so **every** reference
to it stays unresolved — in the most common production Terraform shape
(multi-module repos), recall collapses toward zero.

There is also a fourth failure the naive design misses entirely: the
**incremental rebind path** in `crates/spur-graph/src/store/build.rs` is a second,
independent resolver. `rebind_remaining_edges` (`build.rs:1392`) re-resolves
cross-file edges after incremental builds; for `References` edges,
`rebind_candidate_kinds` returns `None` (`build.rs:1998-2005`), so **all symbol
kinds are candidates** (`build.rs:1440-1447`) and only `method`/`function`
targets get directory/crate safety checks (`build.rs:1473-1494`). Any design that
guards only `resolve_pending_edges` regresses on the first incremental rebuild.

### 1.2 Design

**Capture channel.** A new channel, `@hcl_ref`, in `queries/hcl/spur-edges.scm`.
It captures the expression node rooting a `variable_expr` + `get_attr` chain
(the verified grammar shape: `variable_expr (identifier)` followed by sibling
`get_attr (identifier)` and `index` nodes), in both bare-expression position and
inside `template_interpolation` (so `"${var.name}"` in `string_lit` templates is
covered — this matters for pre-0.12-style code).

**Emission.** A new `"hcl_ref"` arm in `emit_edges`
(`languages.rs:966-1135`), sibling to the `"reference.name"` arm at
`languages.rs:1116`. Unlike that arm, it does not take the raw capture text: it
walks `capture.node` (available on `CaptureHit`, `tree_sitter.rs:58-64`) to
collect the identifier segments of the chain, skipping `index` nodes, then
truncates to the **canonical Terraform address** by first-segment rule:

| First segment | target_name emitted | Segments kept |
|---|---|---|
| `var` | `var.<name>` | 2 |
| `local` | `local.<name>` | 2 |
| `module` | `module.<name>` | 2 (output tails trimmed; binds to the module block) |
| `data` | `data.<type>.<name>` | 3 |
| `count`, `each`, `self`, `path`, `terraform`, `provider` | **no edge** (Terraform builtins) | — |
| anything else with ≥ 2 segments | `<type>.<name>` (resource address) | 2 |
| single bare identifier | **no edge** (for-expression loop vars, locals-internal names) | — |

The arm pushes a `PendingEdge` (`tree_sitter.rs:26-35`) with:
- `relation: RelationKind::References` (semantically correct — these are value
  references, not calls; no new `RelationKind`, per roadmap preference)
- `edge_kind: Some(GraphEdgeKind::ReferencesAddress)` — **new variant**
- `target_name`: the canonical address
- `import_path`/`receiver_text`/`scope_text`: `None`

**Why a new `GraphEdgeKind` variant and not `bind_method` metadata:**
`bind_method` is provenance of a *successful* bind and does not exist on
unresolved edges, so it cannot drive resolution guards. `edge_kind` is carried on
the `PendingEdge`, persisted in `GraphEdgeArtifact`
(`crates/spur-graph/src/schema.rs:193`), and round-trips through the Parquet
codec (`crates/spur-graph/src/store/parquet.rs:2917-2935`) — it is the only
durable discriminator that **both** resolvers (in-memory and incremental rebind)
can key on. `GraphEdgeKind` is a closed serde enum (`schema.rs:427-434`), so the
new variant is a compatibility event — but Phase 1b already requires a
`SCHEMA_VERSION` bump for `NodeKind::Resource` (§2), and both ride the same bump.
`graph_edge_kind_or_default` (`schema.rs:436-445`) is unaffected because the
kind is always explicit.

**Resolution path.** A dedicated branch at the top of the `References` arm in
`resolve_pending_edges` (`tree_sitter.rs:691`), taken when
`edge.edge_kind == Some(GraphEdgeKind::ReferencesAddress)`:

1. Candidates: `builder.symbol_index.get(&edge.target_name)`
   (`tree_sitter.rs:53`; the full candidate list, not just singletons — this is
   what defeats failure (3); precedent: `resolve_call_edge_after_qualified_miss`
   already reads `symbol_index` directly at `tree_sitter.rs:826`), filtered by:
   - `target != edge.source`
   - **target-kind allowlist**: `NodeKind::Resource | NodeKind::Constant`
     (via `indexes.node_kind_by_id`, `tree_sitter.rs:157`; precedent for
     kind-allowlisted relational binds: `relational_target_kinds`,
     `tree_sitter.rs:1305-1311`)
   - **language guard**: `language_family` of the target's file equals that of
     the source's file (via `indexes.file_by_id`, `tree_sitter.rs:156`, built at
     `tree_sitter.rs:612-630`; precedent: `relational_symbol_candidates` already
     applies a source/target family filter at `tree_sitter.rs:1368-1376`).
     `language_family` (`tree_sitter.rs:2799-2812`) gains `"tf" | "hcl" =>
     "hcl"`. This plus the kind allowlist defeats failure (2): a Markdown
     `Section` fails both checks.
2. **Module-scope pass** (defeats failure (3)): among filtered candidates, those
   whose file directory equals the source file's directory. Exactly one →
   bind with `bind_method: "address_module_scope"`.
3. **Workspace fallback**: if the module-scope pass yields zero, and the filtered
   workspace-wide set has exactly one → bind with
   `bind_method: "address_singleton"`.
4. Otherwise (zero or ≥ 2): emit the edge **unresolved** (`target: None`).
   Unresolved labels remain valid evidence (roadmap Backward Compatibility,
   `docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md:140-141`)
   — this branch never silently drops, unlike the ReferencesHof path.

`reference_fallbacks_allowed` (`tree_sitter.rs:810-815`) is untouched: it only
inspects `ReferencesHof`, and address edges never reach it. HOF reference
behavior for Rust/Python/C++/TS is byte-for-byte unchanged.

The two new `bind_method` stamps are added to the documented vocabulary on
`GraphEdgeArtifact::bind_method` (`schema.rs:194-201`). Confidence: address
binds flow through `confidence_for_edge`'s `References` default of
`(Heuristic, 0.5)` (`tree_sitter.rs:3047-3061`); an optional refinement is a
`ReferencesAddress` arm returning `(Heuristic, 0.8)` for `address_module_scope`
binds, mirroring how `metadata_for_pending_edge` adjusts call confidence
(`tree_sitter.rs:3005-3045`) — recommended but severable.

**Incremental rebind parity (the fourth failure).** `rebind_remaining_edges`
(`build.rs:1392`) gets a matching branch keyed on the artifact's
`edge_kind == ReferencesAddress`: filter `symbols_by_entity_name` matches to
`symbol_kind ∈ {"resource", "constant"}`, require the HCL language family on
both endpoints, prefer `same_directory_path` (helper already exists,
`build.rs:2054-2062`, already used for the method same-dir gate at
`build.rs:1473-1478`), else unique workspace match, else unresolved. Without
this, the first incremental rebuild after an edit would rebind address refs
against the unfiltered all-kinds candidate list (`build.rs:1440-1447`).

### 1.3 Recall ceiling (documented, honest)

Upper bound: address references resolve **iff the target address is defined in
the same module directory of the same workspace** (plus the workspace-singleton
fallback). Terraform semantics make this a tight bound rather than a compromise:
a configuration cannot reference another module's resources directly — only via
`module.<name>` addresses, and the `module` block being referenced lives in the
*referencing* directory. So directory scoping loses no legitimate recall.

What resolves: `var.*`, `local.*` (same-module by language rule); resource and
`data.*.*` refs (same-module); `module.X` and `module.X.<output>` (truncated to
the local module block); indexed/splat forms (`aws_instance.web[0].id`,
`module.vpc["a"].x` — `index` nodes are skipped in the walk); refs inside
`${...}` template interpolations.

What never resolves, by design (not counted as recall misses):
- Terraform builtins (`count.index`, `each.*`, `self.*`, `path.*`,
  `terraform.workspace`) — skipped at emission, zero edges.
- Function calls (`templatefile(...)`, `cidrsubnet(...)`) — all Terraform
  functions are builtins; no call channel is emitted (§5).
- Internals of registry/external modules — not in the workspace.

What emits but stays unresolved (bounded noise, kept as evidence):
- Provider alias refs (`provider = aws.west`) — providers are unmodeled in v1
  (§3); these match the resource-address shape and stay unresolved.
- For-expression loop-var attribute access (`[for s in x : s.id]` → `s.id`) —
  matches the 2-segment shape; a v1.1 precision refinement can track
  `for_expr`-bound names during the walk. Negative fixture required either way.

Out of scope, stated in the README matrix: `.tf.json` (JSON syntax; the
extension matcher sees `json`), Terragrunt-specific semantics (`terragrunt.hcl`
parses as generic HCL; its `dependency`/`include` vocabulary emits no false
binds because guards hold, but gets no dedicated modeling).

Estimated realized recall on an idiomatic multi-module, all-local Terraform
repo: **~85–95% of in-workspace address references bind**, with the residue
being provider refs, loop-var captures, and exotic dynamic-block splats — all
left unresolved, never wrongly bound. This satisfies the roadmap's "recall is
measured and documented; intentionally partial gets a scoped note"
(roadmap:175-177).

---

## 2. `NodeKind::Resource` Promotion

Evaluated against the six promotion criteria at
`docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md:60-73`:

| # | Criterion | Verdict | Grounds |
|---|---|---|---|
| 1 | Durable identity across edits | **Pass** | The Terraform address (`aws_instance.web`, `module.vpc`, `data.aws_ami.ubuntu`) is the durable identity Terraform itself uses for state addressing; it survives body edits and file moves within a module. Stable IDs add `relative_path` + fqn + kind + start byte (`tree_sitter.rs:401-406`). |
| 2 | Directly queried by agent workflows | **Pass** | "Where is `aws_instance.web` defined / what references it" is a first-order code-exploration and blast-radius query (roadmap Workflow Anchor:37-39); infra review ("what does changing this security group hit") queries the Resource node itself, not an enclosing symbol. |
| 3 | Needs own edges | **Pass** | Inbound `references` edges from other resources/locals/outputs are the entire point of §1; outbound `contains` from `File` and `defines` per the standard emission path (`languages.rs:919-921`). The §1 target-kind allowlist *requires* a discriminable kind — this is criterion 6's twin. |
| 4 | Consistent scope across target languages | **Pass** | One grammar, two extensions; address semantics are uniform across `.tf`/`.hcl` and scoped by module directory (§4). No cross-language alignment problem exists because the kind is deliberately narrow to the HCL family. |
| 5 | No high-cardinality noise | **Pass** | One node per written `resource`/`data`/`module` block — cardinality is the same order as `Function` in code files. Attributes inside blocks are NOT promoted (only `locals` attributes become `Constant`s, matching how code constants are modeled). |
| 6 | Existing kinds/labels/metadata cannot answer with comparable quality | **Pass** | Folding into `Struct` breaks the §1 resolver guard: `Struct` is a legal target for `constructs` and import resolution (`tree_sitter.rs:1313-1320`, `tree_sitter.rs:2712-2727`), so every workspace `Struct` would become a legal address-bind target, reintroducing phantom binds; and `symbol_kind` is derived 1:1 from `NodeKind` (`languages.rs:1625-1644`, `build.rs:2344-2346`), so a "struct with a resource label convention" cannot be told apart in SQL/MCP queries without string-prefix hacks. |

**Verdict: promote.** Narrowly: exactly `resource`, `data`, and `module` blocks
map to `NodeKind::Resource`. `variable`/`output`/`locals` map to the existing
`NodeKind::Constant` (dropped-`Variable` decision), with address labels
`var.<name>` / `output.<name>` / `local.<name>` — same reuse pattern as Rust
`const_item`/`static_item` → `Constant` (`languages.rs:417`).

Schema changes:
- `NodeKind` enum: add `Resource` variant (`schema.rs:259-281`; serde is
  name-based `snake_case`, so append-order is free).
- Discriminator: `Self::Resource => "resource"` in `NodeKind::discriminator`
  (`schema.rs:283-309`). This automatically flows to artifact `symbol_kind`
  strings via `build.rs:2344-2346`.
- Extractor `symbol_kind`: `NodeKind::Resource => "resource"` in the
  `languages.rs` copy (`languages.rs:1625-1644`) — required by the gate assert
  that no mapped kind falls back to `"symbol"` (`languages.rs:1868-1874`).
- `definition_rank` (`languages.rs:1592-1607`): the `_ => 10` default suffices;
  Resource blocks do not overlap-capture with other definition kinds on the same
  node, so dedup ordering never sees them compete.
- **`SCHEMA_VERSION` bump**: `"spur-graph-schema-v9"` → `"spur-graph-schema-v10"`
  (`build.rs:31`). This feeds `current_manifest_version`
  (`build.rs:192-199`) and forces the full rebuild on version change
  (`build.rs:313-318`), satisfying the roadmap's compatibility contract for new
  enum vocabulary (roadmap:130-135, 145-154). The `GraphEdgeKind::ReferencesAddress`
  variant (§1) and the Parquet codec arms (`parquet.rs:2917-2935`) ride the same
  bump.

---

## 3. Address Builder

**Problem.** `block` children are positional with NO field names (verified
grammar: `identifier`, `string_lit`*, `body`, `block_start`, `block_end` in
document order), and the canonical address joins the block keyword class with
one or two **unquoted** labels (`resource "aws_instance" "web"` →
`aws_instance.web`). The generic definition pipeline expects one inner `@name`
capture per definition (`languages.rs:911-914`) and takes its raw text
(`languages.rs:1495-1497`) — it cannot assemble a joined, unquoted address.

**Mechanism: extend `definition_name`, not a new extractor hook.** The exact
precedent already exists: the `Impl` arm of `definition_name`
(`languages.rs:1482-1493`) assembles a composite name from two contained
captures (`impl.trait` + `impl.self`) via `contained_capture_text`
(`languages.rs:2303-2319`). HCL adds arms keyed on the capture name
(`CaptureHit.name`, `tree_sitter.rs:60`), which `definition_name` receives:

| Capture (tags.scm) | NodeKind | Contained captures | Label/FQN built |
|---|---|---|---|
| `@definition.resource` | `Resource` | `@resource.type`, `@resource.name` (the two `string_lit`s) | `<type>.<name>` |
| `@definition.data` | `Resource` | `@resource.type`, `@resource.name` | `data.<type>.<name>` |
| `@definition.module` | `Resource` | `@resource.name` (one `string_lit`) | `module.<name>` |
| `@definition.variable` | `Constant` | `@resource.name` | `var.<name>` |
| `@definition.output` | `Constant` | `@resource.name` | `output.<name>` |
| `@definition.local` | `Constant` | `@name` (attribute `identifier`) | `local.<name>` |

Many-to-one capture→kind mapping is legal: `definition_kind` looks up by capture
name (`languages.rs:1407-1416`) and the gate test only checks set-equality
between compiled `@definition.*` captures and `definition_kind_map` keys
(`languages.rs:1850-1866`).

**Query shapes** (anchored positional matching; `#eq?`/`#match?` predicates are
already in use, e.g. `queries/rust/spur-edges.scm:72`):

```scheme
(block (identifier) @_kw
  . (string_lit) @resource.type
  . (string_lit) @resource.name
  (body)) @definition.resource
(#eq? @_kw "resource")
```

with sibling patterns for `data` (same shape, different keyword), `module` /
`variable` / `output` (one `string_lit`), and the **locals explosion**:

```scheme
((block (identifier) @_kw
   (body (attribute (identifier) @name) @definition.local))
 (#eq? @_kw "locals"))
```

Each `attribute` node becomes its own `Constant` — distinct byte ranges, so the
dedup in `definition_candidates` (`languages.rs:1465-1472`) keeps them all. The
FQN comes from `scoped_name`/`fqn_segment` as usual (`languages.rs:917`,
`languages.rs:2349-2355`); top-level blocks have the file as parent
(`nearest_parent`, `languages.rs:1500-1517`), so FQN == address.

**String unquoting.** `string_lit` wraps `quoted_template_start/end` around a
`template_literal`. The `definition_name` arm extracts the inner
`template_literal` child's text via a small helper beside `child_text`
(`languages.rs:2345-2347`). If a label contains a `template_interpolation`
(dynamic label — invalid Terraform anyway), the helper returns `None` and the
definition is skipped, which is exactly the established missing-label behavior
(`languages.rs:911-914`).

**Provider aliases.** `provider "aws" { alias = "west" }` is deliberately
unmodeled in v1: no definition capture, and `aws.west` references (which match
the resource-address shape) stay as unresolved References evidence. Modeling
providers would require either a fourth Resource sub-class or alias-attribute
body inspection; deferred until a workflow demands it. Documented in the README
notes with a negative fixture.

**Why not a post-process hook** (the `emit_rust_dyn_trait_edges` pattern,
`languages.rs:1223-1263`, called from `extract_file_contents_from_tree` at
`tree_sitter.rs:3254-3256`): hooks bypass the query pipeline, which would exempt
HCL definitions from the gate contract (compiled captures ↔
`definition_kind_map` ↔ README matrix, `languages.rs:1803-1889`) and from
`MANIFEST_QUERY_BYTES`-driven cache invalidation (`build.rs:43-159`). Keeping
definitions in `tags.scm` preserves both. The only Rust additions are the
`definition_name` arms, the unquote helper, and the `emit_edges` `"hcl_ref"` arm
(§1) — **no new hook in `extract_file_contents_from_tree`**.

---

## 4. Module Scoping

**Scoping key: the defining file's parent directory.** A Terraform module *is*
a directory of `.tf` files — the language's own scope boundary, not a synthetic
one. Two modules defining `aws_instance.web` live in different directories by
construction; duplicate addresses within one directory are invalid Terraform and
are treated as ambiguous → unresolved (matching existing ambiguous handling,
e.g. `tree_sitter.rs:742-750`).

**How it threads — no FQN prefix, no `PendingEdge` change:**
- **Identity** already disambiguates: `stable_symbol_id_for` hashes
  `relative_path` alongside fqn/kind/byte (`tree_sitter.rs:401-406`), so two
  `aws_instance.web` nodes in different modules never collide on stable ID.
- **Display/FQN stays the bare address** (`aws_instance.web`), which is what
  humans and agents search for. Prefixing FQNs with a module path would leak an
  encoding into every query surface for a problem the resolver can solve
  locally.
- **Resolution** derives directories at resolve time from maps that already
  exist: `indexes.file_by_id` (`tree_sitter.rs:156`, built `612-630`) gives the
  source and candidate file paths in `resolve_pending_edges`;
  `same_directory_path` (`build.rs:2054-2062`) gives the same in the rebind
  path. The §1 branch prefers same-directory candidates, then unique workspace
  candidates, then leaves the edge unresolved.

**Recall/precision tradeoff, stated:** the module-scope pass is precision-max
and costs no legitimate recall (Terraform references cannot cross module
directories except through `module.<name>` addresses, whose defining block is in
the referencing directory — see §1.3). The workspace-singleton fallback trades a
small precision risk (unusual layouts: a "module" split across directories via
symlinks, or generated trees) for recall in flat single-module repos; the two
cases are distinguishable in the artifact by `bind_method`
(`address_module_scope` vs `address_singleton`), so consumers can weight them,
and the fallback can be tightened later without a schema change. Ambiguity in
either pass always resolves to "unresolved evidence", never a guess.

---

## 5. Schema/Code Change List (ordered)

TDD cadence applies: each behavioral change lands as a `test(...)` commit then a
`fix/feat(...)` commit (repo Testing Guidelines). Serialized-enum additions get
round-trip tests modeled on `schema.rs`'s `change_kind_tests`
(`schema.rs:1042-1099`).

1. **`crates/spur-graph/Cargo.toml`** — add `tree-sitter-hcl = "1.1.0"`
   (one grammar, two extensions; justification recorded per the workspace
   dependency rule). *Config work.*
2. **`schema.rs`** — `NodeKind::Resource` (`schema.rs:259-281`) + discriminator
   arm (`schema.rs:283-309`); `GraphEdgeKind::ReferencesAddress`
   (`schema.rs:427-434`); extend the `bind_method` doc vocabulary
   (`schema.rs:194-201`) with `address_module_scope` / `address_singleton`;
   round-trip serde tests. *Schema.*
3. **`store/parquet.rs`** — `edge_kind_to_str`/`edge_kind_from_str` arms for
   `references_address` (`parquet.rs:2917-2935`). Node kinds need no codec
   change (persisted as `symbol_kind` strings via `build.rs:2344-2346`).
   *Schema-adjacent.*
4. **`store/build.rs`** — bump `SCHEMA_VERSION` v9→v10 (`build.rs:31`); add two
   `MANIFEST_QUERY_BYTES` entries (`hcl`/`tags`, `hcl`/`spur-edges`) to the
   table at `build.rs:43-159`; add the `ReferencesAddress` branch to
   `rebind_remaining_edges` (`build.rs:1392`, beside `build.rs:1436-1478`) using
   the `{"resource","constant"}` allowlist + HCL family check +
   `same_directory_path` preference (`build.rs:2054-2062`). *Resolver (rebind
   side) + config.*
5. **`queries/hcl/tags.scm` + `queries/hcl/spur-edges.scm`** — new files: the six
   definition patterns of §3 and the `@hcl_ref` patterns of §1 (bare expressions
   and `template_interpolation`). *Pure query work.*
6. **`extract/languages.rs`** —
   - `Language::Hcl` and `Language::Terraform` variants (`languages.rs:21-34`),
     both returning the `tree-sitter-hcl` grammar from `tree_sitter_language`
     (`languages.rs:314-329`; precedent: `Javascript`/`Tsx` share
     `LANGUAGE_TSX` at `languages.rs:320,325`);
   - shared `HCL_QUERIES` const + `hcl_config_for` factory (precedent:
     `TYPESCRIPT_QUERIES` + `typescript_config_for`,
     `languages.rs:454-464,498-511`); `config()` arms (`languages.rs:331-346`);
   - `builtin_method_names` → `&[]` (`languages.rs:349-360`; no call channel —
     all Terraform functions are builtins, so no `call` captures are emitted at
     all);
   - `label()` arms `"hcl"` / `"terraform"` (`languages.rs:362-377`);
   - matchers + two `language_registry()` rows, extensions `["hcl"]` and
     `["tf"]` (`languages.rs:778-865`; uniqueness enforced by
     `assert_registry_extensions_are_unique`, `languages.rs:1925-1950`;
     `all_supported_extensions` picks them up automatically,
     `languages.rs:867-872`);
   - `definition_kind_map` with the six §3 capture names;
   - `definition_name` HCL arms + string-unquote helper
     (`languages.rs:1476-1498`, helper beside `languages.rs:2345-2347`)
     — **Rust extractor work (arm-level, not a new hook)**;
   - `emit_edges` `"hcl_ref"` arm with chain walk, address truncation, and the
     reserved-root skip set (`languages.rs:966-1135`) — **Rust extractor work
     (arm-level, not a new hook)**;
   - `symbol_kind` arm `NodeKind::Resource => "resource"`
     (`languages.rs:1625-1644`). *Config + two localized extractor arms.*
7. **`extract/tree_sitter.rs`** — `language_family` gains
   `"tf" | "hcl" => "hcl"` (`tree_sitter.rs:2799-2812`); the `ReferencesAddress`
   branch in the `References` arm of `resolve_pending_edges`
   (`tree_sitter.rs:691-716`) implementing §1's four-step resolution; optional
   confidence refinement in `confidence_for_edge`
   (`tree_sitter.rs:3047-3061`). *Resolver (in-memory side).*
8. **Gate-test rows** (`languages.rs` `gate_contract` module) —
   `expected_definition_captures` entries for `hcl` and `terraform`
   (`languages.rs:2227-2296`); `expected_relation_predicates` rows
   `{contains, defines, references}` for both labels (`languages.rs:2091-2204`);
   `relation_kind_for_edge_capture` test-side dispatch gains `"hcl_ref" =>
   References` in lockstep with `emit_edges` (`languages.rs:2037-2061`, per the
   lockstep comment at `languages.rs:2041-2043`). *Test/config.*
9. **`queries/README.md`** — new `resource` column in the definition matrix
   (`README.md:57-70`; `-` for all existing rows, `Y` for Hcl/Terraform, plus
   `constant` = `Y`); Hcl/Terraform columns in the relation matrix
   (`README.md:106-121`: `contains`/`defines`/`references` = `Y`, rest `—`);
   notes covering address labels, the reserved-root skip set, provider
   non-modeling, `.tf.json` exclusion, and the recall ceiling; walk the "Adding
   A New Language Family" checklist (`README.md:165-190`). *Docs (contract).*
10. **Fixtures + integration tests** — multi-module fixture with duplicate
    `aws_instance.web` addresses (asserts `address_module_scope` binds and
    unresolved-on-ambiguity); negative fixtures: builtin roots
    (`count`/`each`/`self`/`path`/`terraform`), for-expression loop vars,
    provider alias refs, and a Markdown section literally titled
    `aws_instance.web` (asserts no phantom bind — failure (2) regression
    guard); stable-ID and round-trip tests for `Resource`; locals-explosion
    and `template_interpolation` reference coverage. *Tests.*

**New-hook vs. query/config summary:** no new extractor hook is required.
Changes are: pure query/config (items 1, 4-partial, 5, 6-most, 8, 9), two
localized `languages.rs` arms (`definition_name`, `emit_edges "hcl_ref"`), one
resolver branch in each of the two resolvers (items 4, 7), and the schema/codec
additions (items 2, 3).

---

## Verdict

**(a) Go/no-go: GO.** Phase 1b is feasible as scoped. Every mechanism it needs
has an in-tree precedent: composite definition names (`Impl`,
`languages.rs:1482-1493`), shared-grammar language variants
(`Javascript`/`Tsx`, `languages.rs:320-325`), kind-allowlisted resolution
(`relational_target_kinds`, `tree_sitter.rs:1305-1311`), language-family bind
guards (`relational_symbol_candidates`, `tree_sitter.rs:1368-1376`), and
directory-scoped rebinding (`same_directory_path`, `build.rs:1473-1478`). The
gating risk — the reference channel — is resolved by the
`ReferencesAddress` edge kind + target-kind/language guards + directory-first
resolution, applied symmetrically in both resolvers. The cost is one
`SCHEMA_VERSION` bump carrying two enum variants, which the roadmap's
compatibility rules explicitly provide for.

**(b) Single biggest open question:** whether `.hcl` files (Terragrunt, Nomad,
Consul, Packer) should share the full Terraform address vocabulary or get a
reduced generic-HCL treatment. This spec ships shared behavior (the §1 guards
make wrong binds unlikely — unmatched vocabularies just produce fewer captures),
but it is the one place the `Hcl`/`Terraform` variant split could need to
diverge: if Terragrunt corpora show meaningful false-address noise (e.g.
`dependency.vpc.outputs.x` truncating to a bogus `dependency.vpc` address),
`Language::Hcl` would need its own query set or an extra reserved root. A small
corpus measurement during implementation settles it cheaply.

**(c) Recommended task breakdown** (each a TDD pair; DAG order):

1. `feat(spur-graph)` schema: `NodeKind::Resource` +
   `GraphEdgeKind::ReferencesAddress` + parquet codec + `SCHEMA_VERSION` v10
   (items 2, 3, 4-version; round-trip tests first).
2. `feat(spur-graph)` language wiring: dependency, `Language::Hcl`/`Terraform`,
   matchers, registry, config factory, labels, `symbol_kind` arm (items 1,
   6-wiring; gate tests updated in the same pair, item 8-partial).
3. `feat(spur-graph)` definitions: `tags.scm` + `definition_kind_map` +
   `definition_name` arms + unquote helper + locals explosion (items 5-tags,
   6-defs; definition-matrix gate rows).
4. `feat(spur-graph)` reference emission: `spur-edges.scm` `@hcl_ref` +
   `emit_edges` arm + address truncation + reserved roots +
   `MANIFEST_QUERY_BYTES` (items 4-manifest, 5-edges, 6-emit; relation-matrix
   gate rows).
5. `feat(spur-graph)` resolution: `language_family` + `ReferencesAddress`
   branch in `resolve_pending_edges` + confidence refinement (item 7; negative
   fixtures for phantom binds land first).
6. `feat(spur-graph)` rebind parity: `ReferencesAddress` branch in
   `rebind_remaining_edges` (item 4-rebind; incremental-rebuild fixture first).
7. `docs(spur-graph)` + fixtures: README matrices/notes, multi-module and
   negative integration fixtures, recall documentation (items 9, 10).

Tasks 3 and 4 are independent after 2; tasks 5 and 6 depend on 4; task 7 closes.
