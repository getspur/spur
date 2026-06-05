# Recover same-crate singleton-method calls (Tier-1 T1.b.1, recall) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** graph-grounded (code-explore over the full bare-`Calls` resolution chain + spur-analyst
structural characterization + first-principles design), live artifact **`2256c91d`** / manifest
**`93271bef`** (post-v7). Companion to the Tier-1 binding-precision spec
(`docs/superpowers/specs/2026-06-05-code-graph-binding-precision-tier1-design.ipynb`, Frontier B).

## The opportunity (exact, grounded)

The recall crater has a clean, recoverable seam. Of 88k unresolved calls, **2,810** have a bare label
matching exactly one workspace function/method def in the **caller's own crate**. Structural finding:

- **All 2,810 target a lone METHOD** (zero functions — the Function arm of
  `resolve_singleton_bare_target` already binds same-crate singleton functions). The entire
  same-crate recall hole is in the **Method arm**.
- **Strict subset** (label globally unique across *all* symbol kinds, method, same-crate) =
  **2,043 sites / 500 labels** — genuine domain methods (`set_sessions`, `register_delegation`,
  `__test_call_tool`, `append_message`, `insert_atom`, …).
- **All 2,043 are cross-DIRECTORY** (same-directory method calls already resolve). The method is
  defined in a different module of the same crate than the call site — which is exactly why
  `method_scope_matches` (receiver-type text proxy) fails.

**The bug (asymmetry):** `resolve_singleton_bare_target` (`extract/tree_sitter.rs:845`) Method arm:

```rust
Some(NodeKind::Method) => {
    if Calls && method_scope_matches(edge, target, ...) { bind "scope_match" }
    else if Calls { builder.add_pending_edge(edge, None); }   // ← DROP, even for a same-crate lone method
    else { bind }
}
```

The Function arm immediately below it binds same-crate singletons via `function_singleton_safe`. The
Method arm has **no same-crate fallback** — so a globally-unique method called from a sibling module
of the same crate is dropped, even though it is the only possible target.

**The precision wrinkle (why a naive fix is wrong):** a *method* call `x.foo()` dispatches on the
receiver's type, which can be a std/third-party type the closed-world graph never parsed. Binding
*any* same-crate singleton method re-admits **std-name collisions**: measured **7 labels / 97 sites**
(`to_path_buf`, `replace`, …) where the lone workspace method name shadows a std method that is the
real callee. So the gate must be **same-crate AND not a known std/core method name**, and the bind
must carry honest, downgradeable provenance.

## The fix (minimal, precedented, zero rebind-logic change)

`rebind_cross_file_edges` (`store/build.rs:1000`) **skips** any edge that is already
`target.is_some() && bind_method ∈ {fqn, scope_match, singleton, macro_body_singleton, relational}`.
So a resolver bind stamped with a **new** `bind_method` that we add to that set is preserved
untouched — the existing `(method && !same_directory_path)` drop-guard is **not modified**, and the
1,187 cross-crate method phantoms stay gated exactly as today. This is the same protection contract
`scope_match` already enjoys.

📌 **Golden artifacts:** corpus fixtures use non-`crates/` paths (`src/lib.rs`), so
`function_singleton_safe` returns false (non-crate scope) → the new arm **never fires in the corpus**
→ expect **zero golden changes** (as in v7). Bless is run only to confirm none/drops-only.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task method-crate-singleton-recall: bind same-crate globally-unique singleton methods on scope-match miss

**Task ID:** `task-method-crate-singleton-recall`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the Method arm of `resolve_singleton_bare_target` + a `STD_PRELUDE_METHOD_NAMES` const + a unit test in the `tests` module)
- Modify: `crates/spur-graph/src/store/build.rs` (add the new `bind_method` to the `resolution_is_stamped` set in `rebind_cross_file_edges`; `RESOLVER_VERSION` bump → v8)
- Modify: `crates/spur-graph/tests/artifact_range_invariants.rs` (new artifact-level survival test)
- Regenerate (bless, only if changed): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`

**Depends on:** none

**Acceptance Criteria:**
- [ ] In the `Method` arm of `resolve_singleton_bare_target`, when `edge.relation == Calls` and
      `method_scope_matches` is **false**, bind the (already-singleton) target **iff**
      `function_singleton_safe(src_file, tgt_file)` is true **AND** the target's `entity_name` is
      **not** in `STD_PRELUDE_METHOD_NAMES`. Stamp `bind_method = "method_crate_singleton"`. When the
      gate fails (cross-crate, non-crate, or a std-name), drop exactly as today
      (`add_pending_edge(edge, None)`). Resolve the two file paths with the SAME `file_for_node`
      pattern the Function arm uses (so missing file info ⇒ treat as unsafe ⇒ drop, never bind).
- [ ] `STD_PRELUDE_METHOD_NAMES` is a `const`/`static` set of common std/core/trait method names
      (at minimum the ones that collide live: `clone, to_string, to_owned, to_path_buf, to_vec,
      as_str, as_ref, as_mut, as_path, as_bytes, len, is_empty, iter, iter_mut, into_iter, next,
      push, push_str, pop, insert, remove, get, get_mut, contains, contains_key, replace, take,
      into, from, default, eq, cmp, fmt, deref, borrow, borrow_mut, read, write, flush, lock, send,
      recv, build, parse, unwrap, map, filter, collect, count, find, extend, drain, clear, truncate,
      split, join, trim, starts_with, ends_with, keys, values, entry, or_insert, or_default,
      and_then, unwrap_or, expect, ok_or, retain, sort, dedup, first, last, nth, chars, bytes, lines,
      to_lowercase, to_uppercase, min, max`). A short doc-comment must state it is a precision
      heuristic, not an exhaustive std list.
- [ ] `rebind_cross_file_edges` (`store/build.rs`) adds `"method_crate_singleton"` to the
      `resolution_is_stamped` match set so the new bind is preserved (resolved+stamped ⇒ skip). **Do
      NOT** modify the `(method && !same_directory_path) || (function && !function_singleton_safe)`
      drop-guard, the `rebind_candidate_kinds`, the Imports path, or any other relation.
- [ ] A unit test (beside `singleton_function_call_respects_crate_safety`) drives `build_facts` on a
      **two-module, same-crate** fixture and proves: a cross-directory same-crate globally-unique
      method call resolves with `bind_method = "method_crate_singleton"`; **and** in the same test —
      a **cross-crate** singleton method stays unresolved, and a same-crate singleton method named
      `clone` (in the denylist) stays unresolved. (Use `crates/foo/src/a.rs` def vs
      `crates/foo/src/sub/b.rs` call so the receiver scope text ≠ the impl scope.)
- [ ] An artifact-level test in `tests/artifact_range_invariants.rs` (mirroring the v6/v7 tests)
      proves the recovered same-crate method bind **survives `artifact_from_facts`** (i.e. the rebind
      stamped-skip works): the `calls` edge has `target_stable_symbol_id = Some(_)` and
      `bind_method = "method_crate_singleton"` after assembly.
- [ ] The `assembled_artifact_has_no_out_of_range_resolved_edges` invariant test still passes
      (the new bind targets a `method`, which is in the `Calls` allowed set — no range violation).
- [ ] Goldens re-blessed if changed; expected **no change** (corpus is non-crate-scoped). If any
      golden changes, it MUST be **adds-only of resolved `calls` targets with
      `bind_method="method_crate_singleton"`** — if anything else changes, STOP and emit `risk`.
- [ ] `RESOLVER_VERSION` bumped to `"2026-06-05-method-crate-singleton-recall-v8"`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** unit/artifact test confirmation, whether any golden changed (expect none), and v8
      confirmation. (Live recall delta — expected ~+1,946 `calls` edges — is verified post-rebuild by
      the brain, not in this task.)

**Suggested Worker:** codex.

**Scope Boundary:** IN: the Method-arm `else if Calls` fallback + the `STD_PRELUDE_METHOD_NAMES` const
+ the `resolution_is_stamped` string addition + `RESOLVER_VERSION` + a unit test + an artifact test +
conditional bless. OUT: the rebind drop-guard logic, the cross-crate `scope_match` METHOD gate
(separate Tier-1 item — needs receiver-type discrimination), the Function/References arms (done
v6/v7), `method_scope_matches`/`method_scope_candidates`/`same_directory_path`/`function_singleton_safe`
internals (reuse only), import resolution, the qualified/dyn paths, other relations, other crates,
`schema.rs`. Non-`crates/` (Python/TS) files are out by construction (non-crate scope ⇒ gate false).

**Implementation:**

- [ ] **Step 1: Failing tests.** In the `tests` module of `tree_sitter.rs`, add
  `method_crate_singleton_recovers_cross_module_same_crate`: write `crates/foo/src/a.rs` defining a
  type with a globally-unique method (e.g. `Widget::repaint_panel`), and `crates/foo/src/sub/b.rs`
  with a function that calls it on a non-`self`, non-`Widget`-typed receiver (so scope-match fails).
  Assert the `references`… no — the `calls` edge for `repaint_panel` has `target_node_id = Some(_)`
  and `bind_method = Some("method_crate_singleton")`. Add two negatives in the same test: a method in
  `crates/bar/src/lib.rs` called from `crates/foo` stays `None`; a same-crate unique method literally
  named `clone` stays `None`. Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
  method_crate_singleton` → expect FAIL.

- [ ] **Step 2: Add the const + the gated fallback.** Add `STD_PRELUDE_METHOD_NAMES` (a `phf`/`&[&str]`
  + `.contains` or a `matches!` — match the crate's existing style; a sorted `const &[&str]` with
  `binary_search` is fine). Rewrite the Method arm:

```rust
Some(NodeKind::Method) => {
    if edge.relation == RelationKind::Calls
        && method_scope_matches(edge, target, indexes.enclosing_scope_by_id)
    {
        builder.add_pending_edge_with_bind_method(edge, Some(target), Some("scope_match"));
    } else if edge.relation == RelationKind::Calls {
        // T1.b.1 recall: a globally-unique method called from a sibling module of the SAME crate
        // has exactly one possible target. Bind it, unless the name shadows a std/core method
        // (receiver could be an external type the closed-world graph never parsed).
        let file_for_node = /* same closure as the Function arm */;
        let same_crate_safe = matches!(
            (file_for_node(edge.source), file_for_node(target)),
            (Some(src_file), Some(tgt_file)) if function_singleton_safe(src_file, tgt_file)
        );
        let entity = indexes /* entity_name of `target` */;
        if same_crate_safe && !STD_PRELUDE_METHOD_NAMES.contains(&entity) {
            builder.add_pending_edge_with_bind_method(edge, Some(target), Some("method_crate_singleton"));
        } else {
            builder.add_pending_edge(edge, None);
        }
    } else {
        builder.add_pending_edge(edge, Some(target));
    }
}
```

  **Note on the target's `entity_name`:** `resolve_singleton_bare_target` has `target: NodeId`. Use
  the existing index access pattern to get its label/entity_name (the resolver already maps node→kind
  via `indexes.node_kind_by_id`; obtain the name via the same `builder.facts.nodes`/index lookup the
  `file_for_node` closure uses, or fall back to `edge.target_name`, which equals the called label).
  Using `edge.target_name` is acceptable and cheapest — it IS the method name being resolved.
  Run Step 1 → expect PASS.

- [ ] **Step 2b: Preserve the bind through rebind.** In `rebind_cross_file_edges` (`store/build.rs`),
  add `"method_crate_singleton"` to the `resolution_is_stamped` `matches!` set. Nothing else in that
  function changes.

- [ ] **Step 3: Artifact survival test.** In `tests/artifact_range_invariants.rs`, add
  `artifact_preserves_recovered_method_crate_singleton`: same two-module same-crate fixture, run
  `build_facts` → `artifact_from_facts`, assert the `calls` edge for the method has
  `target_stable_symbol_id = Some(_)` and `bind_method = Some("method_crate_singleton")`. Run
  `--test artifact_range_invariants` → expect PASS (proves rebind skip works).

- [ ] **Step 4: Bump `RESOLVER_VERSION`** (`build.rs:29`) →
  `"2026-06-05-method-crate-singleton-recall-v8"`.

- [ ] **Step 5: Bless goldens (expect NONE).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Expect no fixture diff. If any appears, it MUST be adds-only of `calls` targets with
  `bind_method="method_crate_singleton"`; anything else → STOP and emit `risk`.

- [ ] **Step 6: Broad gate + commit** (green except flaky `incremental_ingest`):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test artifact_range_invariants --test calls_range_resolution_edges --test range_resolution_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/artifact_range_invariants.rs crates/spur-graph/tests/fixtures/
git commit -m "feat(spur-graph): recover same-crate singleton-method calls (method_crate_singleton)"
```

  Report: test confirmations, whether any golden changed (expect none), v8 confirmation.

## Self-Review
- **Coverage:** closes the same-crate Method-arm recall hole (all 2,810 pool calls are lone methods;
  ~1,946 clean cross-directory binds recovered) while preserving every phantom gate.
- **Placeholder scan:** concrete arm rewrite + concrete const + concrete unit/artifact tests; bless
  conditional and expected-empty.
- **Type consistency:** `function_singleton_safe` (pub(crate)), `add_pending_edge_with_bind_method`,
  `node_kind_by_id`, the `file_for_node` closure, and the `resolution_is_stamped` set all already
  exist in the touched functions.
- **DAG:** single task.
- **Risk:** additive recall, not a drop. The new bind is (a) gated same-crate by the same predicate
  v6 uses, (b) std-name-denylisted against the measured 97-site collision class, (c) stamped with a
  distinct, filterable `bind_method` so it is honest and reversible, and (d) preserved via the
  existing stamped-skip contract with **no change to the rebind drop-guard** — so the 1,187
  cross-crate method phantoms stay gated. Corpus is non-crate-scoped ⇒ zero golden churn expected.
  The cross-crate method gate and import-aware recall remain explicitly deferred (design-first).
