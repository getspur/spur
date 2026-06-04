# Fix `rebind_cross_file_edges` domain/range bypass Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** empirical root-cause trace (spur-analyst + `code_*` + source read).

**The bug (exact):** `rebind_cross_file_edges` in `crates/spur-graph/src/store/build.rs` (~lines 997-1085,
called from `artifact_from_facts` at ~line 988) is a SECOND resolution pass that runs AFTER
`build_facts`. It re-resolves edges by `target_label` across files. It applies a kind filter **only for
`RelationKind::Imports`** (lines ~1043-1050); for **every other relation** (`extends`/`implements`/
`calls`) it binds to the first same-named symbol of **any kind** (the `else` arm `matches.iter().collect()`
then `edge.target_stable_symbol_id = Some(resolved...)`). So edges the fixed resolver in
`extract/tree_sitter.rs` deliberately left UNRESOLVED (out-of-range `extends→Send`, non-callable
`calls→field`) get RE-BOUND here to a wrong-kind symbol, with empty `bind_method`.

**Proof this is the bug, not cache/binary:**
- Unit tests `range_resolution_edges` / `calls_range_resolution_edges` call `build_facts` directly and
  pass — they never reach this rebind pass.
- The CLI graph (`artifact_from_facts` → rebind) shows the violations.
- A cache-free full rebuild reproduces byte-identical output (deterministic code path).
- Live artifact `3972626c`: `extends`/`implements`→enum_variant/section = 141, `calls`→field = 4710, all
  empty `bind_method` — exactly the unguarded-rebind signature.

**Goal:** Apply the SAME domain/range guards in `rebind_cross_file_edges` that the resolver already
enforces, so the second pass cannot re-introduce out-of-range / non-callable binds.

**Range (mirror the resolver, but `symbol_kind` here is a `&str`/`String`):**
- `Extends`    → `{"trait","interface","class"}`
- `Implements` → `{"trait","interface"}`
- `Calls`      → `{"function","method"}`  (non-callable stays unresolved; the `constructs`
  reclassification lives in the resolver and is out of scope here)
- `Imports`    → existing `is_import_rebind_candidate_kind` (UNCHANGED)
- all other relations → current behavior (unchanged)

📌 **Golden artifacts WILL change** (corpus contains out-of-range/non-callable rebinds that now drop to
unresolved). Re-blessing the 4 corpus goldens is **in scope and budgeted**.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task rebind-guard: domain/range-guard the cross-file rebind pass

**Task ID:** `task-rebind-range-guard`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/store/build.rs` (the `rebind_cross_file_edges` kind guard + the
  stamped-skip set + a unit test; `RESOLVER_VERSION` bump)
- Regenerate (bless, expected): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`
- Modify (only if an assertion legitimately changes): `crates/spur-graph/tests/resolver.rs`,
  `crates/spur-graph/tests/extractor.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] In an artifact produced via `artifact_from_facts` (i.e. AFTER rebind), no `extends`/`implements`
      edge has a resolved target whose `symbol_kind` ∉ its allowed set, and no `calls` edge resolves to a
      non-callable kind (`field`/`module`/`section`/`constant`/`enum`/`macro`).
- [ ] A `trait Foo: Send {}` with a local `enum E { Send }` does NOT rebind `extends→Send` to the enum
      variant; it stays unresolved (`target_stable_symbol_id = None`, label retained).
- [ ] A correctly-resolved in-range relational bind (`bind_method = "relational"`) is preserved by
      rebind (add `"relational"` to the `resolution_is_stamped` set at ~line 1026-1029).
- [ ] In-range local supertrait / interface impl still rebinds/resolves when the only same-named symbol
      is the correct trait/interface.
- [ ] Goldens re-blessed; full `-p spur-graph` suite green except the flaky `incremental_ingest`;
      `gate_contract` + clippy `-D warnings` clean.
- [ ] `RESOLVER_VERSION` bumped to a v4 value (e.g. `"2026-06-05-rebind-range-guard-v4"`) since this
      changes resolution semantics.

**Suggested Worker:** codex.

**Scope Boundary:** IN: `rebind_cross_file_edges` + its test + `RESOLVER_VERSION` + re-blessed goldens.
OUT: `extract/tree_sitter.rs` (the resolver is already correct), `schema.rs`, the temporal/`git_walk`
path, `References` rebinding (leave as-is — not part of the confirmed violation; note it as a possible
follow-up), other crates. Do NOT change `is_import_rebind_candidate_kind` or the Imports path.

**Implementation:**

- [ ] **Step 1: Failing test.** Add a unit test in the `build.rs` test module modeled on the existing
  `rebind_leaves_import_unresolved_when_filter_empties_candidates` (~line 1623). Construct buckets with:
  a source symbol `Foo` (trait) carrying an `extends` edge `target_label="Send"` (unstamped,
  `target_stable_symbol_id=None`), and a symbol `Send` of `symbol_kind="enum_variant"` in another
  bucket. Call `rebind_cross_file_edges(&mut buckets)`. Assert the `extends` edge's
  `target_stable_symbol_id` is still `None` (NOT bound to the enum_variant). Add a positive case: an
  `extends` edge to a `Base` of `symbol_kind="trait"` DOES rebind. Add a `calls`→`field` negative case.
  Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib rebind` → expect FAIL.

- [ ] **Step 2: Add the kind guard.** In `rebind_cross_file_edges`, generalize the Imports-only filter
  into a per-relation allowed-kind filter. Suggested shape:

```rust
fn rebind_candidate_kinds(relation: RelationKind) -> Option<&'static [&'static str]> {
    match relation {
        RelationKind::Extends => Some(&["trait", "interface", "class"]),
        RelationKind::Implements => Some(&["trait", "interface"]),
        RelationKind::Calls => Some(&["function", "method"]),
        _ => None, // Imports handled by is_import_rebind_candidate_kind; others unchanged
    }
}
```

  Then replace the `if Imports { import filter } else { all }` block so that:
  - `Imports` → `is_import_rebind_candidate_kind` (unchanged),
  - a relation with `Some(allowed)` from `rebind_candidate_kinds` → filter `matches` to those kinds,
  - otherwise → current behavior.
  Empty filtered `matches` must leave the edge unresolved (the existing `matches.first()` → `None` path
  already does this).

- [ ] **Step 3: Preserve stamped relational binds.** Add `"relational"` to the `resolution_is_stamped`
  match (~line 1026-1029) so already-correct relational edges are skipped by rebind.

  Run Step 1 again → expect PASS.

- [ ] **Step 4: Bump `RESOLVER_VERSION`** in `build.rs` (~line 26) to `"2026-06-05-rebind-range-guard-v4"`.

- [ ] **Step 5: Re-bless goldens (expected).** Out-of-range/non-callable rebinds drop to unresolved:

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Sanity-check the golden diff: it should ONLY drop resolved targets on `extends`/`implements`
  (out-of-range) and `calls` (non-callable) edges (target → null, label kept). NO `imports`/`contains`/
  `defines`/`constructs` change, and no edge's target changes to a *different* node. If anything else
  changed, STOP and emit `risk`.

- [ ] **Step 6: Broad gate + commit** (green except flaky `incremental_ingest`, do NOT run it):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test range_resolution_edges --test calls_range_resolution_edges --test rust_implements_edge \
  --test rust_extends_edge --test ts_inheritance_edges --test cpp_inheritance_edges --test python_inheritance_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/store/build.rs crates/spur-graph/tests/fixtures/
# plus tests/resolver.rs / tests/extractor.rs if an assertion legitimately changed
git commit -m "fix(spur-graph): domain/range-guard cross-file edge rebind"
```

  Report: the resolved→unresolved delta by relation+target kind, which goldens changed, any test
  assertion updated, and confirmation `RESOLVER_VERSION` was bumped.

## Self-Review
- **Coverage:** closes the rebind bypass that re-introduced out-of-range `extends`/`implements` and
  non-callable `calls` after the resolver correctly left them unresolved.
- **Placeholder scan:** concrete helper + filter wiring + concrete tests; golden re-bless budgeted.
- **Type consistency:** `RelationKind`, `symbol_kind` strings, `is_import_rebind_candidate_kind`,
  `resolution_is_stamped` all already in the same function/module.
- **DAG:** single task.
- **Risk:** resolution-affecting change bounded to the rebind pass; Imports path untouched; diff-is-
  drops-only golden check + `risk` off-ramp for any other-relation change.
