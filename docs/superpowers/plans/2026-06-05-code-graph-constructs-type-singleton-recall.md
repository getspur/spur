# Type-filtered `Constructs` recall (Tier-1 constructor singleton, v11) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** graph-grounded review of the freshly-rebuilt v10 live artifact (content hash
`53b1ece4`, 50,037 nodes, resolver `…per-language-builtins-v10`). Three-phase analysis
(code-explore grounding → spur-analyst structural → first-principles) established that Tier-1
binding **precision is essentially complete** — every crate-safety-gated bind_method
(`singleton`, `method_crate_singleton`, `macro_body_singleton`, `fqn`) has **zero** cross-crate
binds, and `scope_match` is qualifier-disciplined (1,163 / 1,187 cross-crate binds hit a unique
type name; 24-bind collision tail). The next frontier is **recall**, and the tightest, lowest-risk
recall lever is the `Constructs` relation.

**The gap (exact, grounded):** the resolver has relation-specific candidate filters for every
relation **except** `Constructs`:
- `Calls` → `callable_symbol_candidates` (filters to callable kinds)
- `Imports` → `import_resolution_candidates`
- `Implements`/`Extends` → `relational_symbol_candidates` (filters to trait/interface/class)
- `Constructs` → **nothing.**

A `Constructs`-relation edge emitted directly by the extractor (struct-literal / constructor
syntax) matches none of the typed arms in the resolution loop
(`crates/spur-graph/src/extract/tree_sitter.rs`, the `relation`-dispatch starting ~line 568 and
`resolve_bare_pending_edge` ~line 715). It falls through to the generic **all-kinds**
`singleton_symbols_by_label` index (~line 828). That index keys a label only when it is unique
across **all** symbol kinds — so a construction target whose type name collides with a same-named
`field` / `function` / `method` / `module` is treated as ambiguous and left **unresolved**, even
though it is the workspace's **unique constructible type**.

**Live evidence (v10 artifact, unresolved `constructs` edges with a `crates/` source):**

| bucket | unresolved edges | disposition |
|---|---|---|
| **type-unique, all-kinds-ambiguous** | **1,185** | **RECOVERABLE by a type-kind candidate filter** |
| multi-type multi-crate | 333 | genuinely ambiguous — stays unresolved |
| multi-type same-crate | 96 | genuinely ambiguous — stays unresolved |
| clean all-kinds singleton | 14 | already resolves (control) |

Recovering the 1,185 is a ~+125% increase over the 951 currently-resolved `constructs` edges,
densifying the *Subject–constructs–Object* layer of the knowledge graph.

**Why this is the right next inch (same shape as v8 `method_crate_singleton`):**
- **Tight:** one new candidate function + one relation arm, mirroring the existing
  `relational_symbol_candidates` / `relational_target_kinds` pattern verbatim.
- **Low-risk by construction:** only a **unique** constructible-type target binds; the 333+96
  multi-type cases keep >1 candidate → stay unresolved (ambiguous). Zero phantom risk, exactly like
  `singleton`.
- **Cross-crate is correct here (no crate gate):** unlike bare method names, a workspace-unique
  **type** name is globally unambiguous, and constructing an imported type across a crate boundary
  (`use crate_b::Foo; Foo::new()`) is legitimate recall. The *only* cross-boundary hazard is
  language (a TS `new Foo()` must not bind a Rust `struct Foo`), so apply the **v9 language-family
  gate** to the cross-file case — reusing the existing private `language_family` helper.

**Why a type filter, not widening the all-kinds singleton:** widening the generic singleton index
would re-introduce cross-kind misbinds elsewhere. A construction can *only* target a type, so the
principled fix is a relation-scoped candidate set restricted to constructible kinds — identical in
spirit to how `Calls` restricts to callable kinds and `Implements`/`Extends` restrict to
trait/interface/class.

📌 **Golden artifacts:** unlike v9/v10 (which were pure tightenings → zero churn), this **adds**
binds. Within a single-language corpus, any directly-emitted `constructs` edge whose type name is
unique-among-types but all-kinds-ambiguous will now resolve. **Expect NON-zero golden churn** in the
corpora that contain such a case. Bless and report the diff; every new edge must be a `constructs`
edge to a unique constructible-type target in the same language family.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

**Out of scope (explicitly deferred):** import-resolution keystone (Frontier C — 9,668 unresolved
imports; deserves its own design spec), receiver-type method discrimination (Frontier D), the
333+96 genuinely-ambiguous multi-type constructs, and the 14 already-resolving control cases.

---

### Task constructs-type-singleton: type-filtered candidate resolution for the `Constructs` relation

**Task ID:** `task-constructs-type-singleton`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — add `constructs_target_kinds()` +
  `constructs_symbol_candidates()` (mirroring `relational_target_kinds` /
  `relational_symbol_candidates`); add a `Constructs` arm to the resolution loop /
  `resolve_bare_pending_edge` that resolves the unique constructible-type candidate, gated by the
  language-family check for cross-file binds; add unit + behavioral tests.
- Modify: `crates/spur-graph/src/store/build.rs` — `RESOLVER_VERSION` bump → v11.
- Regenerate (bless — **expect non-zero**): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`.

**Depends on:** none

**Acceptance Criteria:**
- [ ] A new `constructs_symbol_candidates(builder, edge, indexes)` returns candidate `NodeId`s for
      `edge.target_name` filtered to constructible kinds **{`Struct`, `Enum`, `EnumVariant`,
      `Class`}**, excluding `edge.source`, sorted+deduped — structurally identical to
      `relational_symbol_candidates`.
- [ ] In the resolution dispatch, a `Constructs`-relation edge resolves to the **single** candidate
      from `constructs_symbol_candidates` when exactly one exists; with >1 candidate it stays
      unresolved (ambiguous, `add_pending_edge(edge, None)`); with zero it stays unresolved.
- [ ] **Language-family gate on cross-file binds:** when the single candidate is in a different file
      from the source, bind only if `language_family(src_file) == language_family(tgt_file)` (both
      `Some(_)`); same-file always binds. Reuse the existing private `language_family` helper (do
      **not** crate-gate — cross-crate unique-type construction is valid recall).
- [ ] Bind method label: stamp the recovered edges with a distinct `bind_method`
      `"constructs_type_singleton"` (so the brain can measure recall and the rebind drop-guard can
      recognize it — see next criterion).
- [ ] **Rebind drop-guard:** add `"constructs_type_singleton"` to the `resolution_is_stamped` skip
      set in `rebind_cross_file_edges` (`store/build.rs`, the set currently containing `fqn`,
      `scope_match`, `singleton`, `macro_body_singleton`, `relational`, `method_crate_singleton`) so
      a recovered cross-file constructs bind is not re-dropped by the rebind pass.
- [ ] **Unit tests** (in the `tests` module): `constructs_type_singleton_binds_unique_type`
      (one constructible-type def, all-kinds-ambiguous name → resolves);
      `constructs_type_singleton_ambiguous_multi_type_unresolved` (two type defs → unresolved);
      `constructs_type_singleton_blocks_cross_language` (a `.ts` construction whose only
      constructible-type def is a Rust `struct` in another file → unresolved).
- [ ] **Behavioral `build_facts` test** proving end-to-end recovery: a fixture where a type name is
      unique among constructible kinds but **collides with a same-named non-type symbol**
      (e.g. a `struct Widget` plus a function/field `Widget`/`widget`), constructed from another
      module/crate; assert the resulting `constructs` edge resolves with
      `bind_method = "constructs_type_singleton"` (this is the exact class of the 1,185 — a control
      that today is left unresolved).
- [ ] No change to `callable_symbol_candidates`, `relational_symbol_candidates`,
      `import_resolution_candidates`, `function_singleton_safe`, the per-language builtin lists, the
      Calls/References/Imports/relational arm bodies, `path_scope`/`path_crate`, or `schema.rs`.
- [ ] Goldens re-blessed; **expect non-zero** churn — every new line must be a `constructs` edge to a
      unique constructible-type target in the same language family. If any NON-`constructs` edge
      changes, or any new `constructs` edge crosses a language family, **STOP and emit `risk`**.
- [ ] `RESOLVER_VERSION` bumped to `"2026-06-05-constructs-type-singleton-v11"`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** new tests pass; the golden diff summary (how many `constructs` edges newly
      resolved, per corpus); v11 confirmation. (Live effect — ~1,185 recovered `constructs` edges —
      is verified post-rebuild by the brain.)

**Suggested Worker:** codex.

**Scope Boundary:** IN: `constructs_target_kinds` + `constructs_symbol_candidates` + the
`Constructs` resolution arm + `constructs_type_singleton` bind label + language-family cross-file
gate + the `resolution_is_stamped` skip entry + `RESOLVER_VERSION` + unit/behavioral tests + bless.
OUT: the import-resolution keystone, receiver-type discrimination, the multi-type ambiguous
constructs, `callable`/`relational`/`import` candidate functions, the singleton-family bind logic,
`function_singleton_safe`, the per-language builtin denylists, `path_scope`/`path_crate`, other
relations, other crates, `schema.rs`.

**Implementation:**

- [ ] **Step 1: Failing tests.** Add the three unit tests + the behavioral `build_facts` test
  above. Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib constructs_type_singleton`
  → expect the unique-type and behavioral tests to FAIL (current code leaves the all-kinds-ambiguous
  construction unresolved).

- [ ] **Step 2: Add `constructs_target_kinds` + `constructs_symbol_candidates`**, mirroring
  `relational_target_kinds` / `relational_symbol_candidates` (~lines 983–1014). Constructible kinds:
  `&[NodeKind::Struct, NodeKind::Enum, NodeKind::EnumVariant, NodeKind::Class]`.

- [ ] **Step 3: Add the `Constructs` arm** in the resolution dispatch (alongside the
  `relational_target_kinds` arm ~line 781, and/or in `resolve_bare_pending_edge`): on a single
  candidate, apply the language-family cross-file gate, then
  `add_pending_edge_with_bind_method(edge, Some(target), Some("constructs_type_singleton"))`;
  on >1 candidate, `*ambiguous_unresolved += 1; add_pending_edge(edge, None)`. Re-run Step 1 →
  expect PASS, with the cross-language test still unresolved.

- [ ] **Step 4: Rebind drop-guard.** Add `"constructs_type_singleton"` to the
  `resolution_is_stamped` set in `rebind_cross_file_edges` (`store/build.rs`).

- [ ] **Step 5: Bump `RESOLVER_VERSION`** (`build.rs:29`) → `"2026-06-05-constructs-type-singleton-v11"`.

- [ ] **Step 6: Bless goldens (expect NON-zero).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Inspect the fixture diff: every changed line must be a `constructs` edge newly gaining a target +
  `bind_method = "constructs_type_singleton"`, within one language family. Any other change → STOP,
  emit `risk`.

- [ ] **Step 7: Broad gate + commit** (green except flaky `incremental_ingest`):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test artifact_range_invariants --test calls_range_resolution_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/fixtures/
git commit -m "feat(spur-graph): recover unique-type Constructs binds (constructs_type_singleton)"
```

  Report: new tests pass, per-corpus golden diff counts, v11 confirmation.

## Self-Review
- **Coverage:** closes the missing `Constructs` candidate-filter gap; recovers ~1,185 real
  constructor edges (unique constructible-type targets) that today lose to same-name non-type
  collisions in the all-kinds singleton index.
- **Placeholder scan:** concrete candidate function + concrete relation arm + concrete language gate
  + concrete unit/behavioral tests; bless conditional and expected-non-zero with a typed diff check.
- **Type consistency:** mirrors the existing `relational_*` pattern; `language_family` already
  present (private, added in v9); `add_pending_edge_with_bind_method` already used by every other
  bind path.
- **DAG:** single task.
- **Risk:** only **unique** constructible-type targets bind (multi-type stays ambiguous → zero
  phantom risk, same guarantee as `singleton`); cross-language blocked by the v9 family gate;
  cross-crate intentionally allowed (valid type-import recall); the rebind drop-guard entry prevents
  re-drop. Goldens change (additive) — the typed diff check is the safety net. No precision path,
  existing bind_method, or other relation is touched.
