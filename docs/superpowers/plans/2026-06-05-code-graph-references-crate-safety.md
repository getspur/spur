# Extend crate-safety gate to References (HOF) singleton binds (Tier-1 T1.b.2-a, part 2) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** graph-grounded follow-up to the v6 `singleton-crate-safety` fix (code-explore over `resolve_pending_edges`, artifact `b322e436…`, indexed HEAD `3a1790d4`).

**The bug (exact, grounded):** `resolve_pending_edges` (`crates/spur-graph/src/extract/tree_sitter.rs`,
the `else if edge.relation == RelationKind::References` arm, ~lines 556-575) has the **same**
cross-boundary phantom bug that v6 just fixed for `Calls` functions — and it is still **un-gated**:

```rust
} else if edge.relation == RelationKind::References {
    if let Some(target) = singleton_symbols_by_label.get(&edge.target_name).copied() {
        if target != edge.source
            && matches!(node_kind_by_id.get(&target).copied(), Some(NodeKind::Function | NodeKind::Method))
        {
            if let (Some(src_file), Some(tgt_file)) = (
                file_by_id.get(&edge.source).copied(),
                file_by_id.get(&target).copied(),
            ) {
                if !function_singleton_safe(src_file, tgt_file) {
                    phantom_blocked_references += 1;        // ← counted…
                }
            }
            self.add_pending_edge(&edge, Some(target));      // …but bound ANYWAY (cross-crate phantom)
        }
    }
}
```

The end-of-function log comment even names it: *"crate-scope gate (measurement-only) would have
blocked phantom singleton binds."* A singleton HOF reference to a bare name (e.g. a function value
passed to `.map(...)`) binds to the lone same-named workspace symbol even across a crate boundary —
the same stdlib/external-collision class v6 eliminated for `calls`.

**Goal:** Mirror v6 for the `References` predicate, in BOTH resolution paths (resolver + rebind), so a
provably-unsafe (cross-crate / non-crate) singleton HOF-reference bind is left **unresolved** instead
of asserted. Same `function_singleton_safe` predicate, same honesty principle.

**Scope note — this is the small, clean completion of the v6 pattern, NOT the big recall-recovery work.**
Two larger Tier-1 items remain explicitly OUT (each needs a design pass first, NOT this task):
- **T1.b.2-b (import-aware recall recovery)** — recovering the ~199 genuine cross-crate function calls
  via backing `imports` edges. Deferred because the import `target_label` is a bare name (e.g. `use
  std::env` → label `env`) that collides with workspace symbol names, so naive name-matching would
  re-introduce phantoms. Needs a design spec.
- **The 1,188 cross-crate `scope_match` METHOD binds** — methods legitimately cross crates (trait
  methods), so a blanket crate guard would over-drop; also needs import/trait-awareness.

📌 **Golden artifacts may change** (corpus HOF references that were cross-file/non-crate drop to
unresolved). References are a small surface (~110 total in the self-graph), so the corpus delta is
likely tiny or empty. Re-blessing the 4 corpus goldens is **in scope and budgeted**; the diff MUST be
**drops-only on `references` edges** (target → null, label kept).

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task references-crate-safety: gate the singleton HOF-reference bind on `function_singleton_safe`

**Task ID:** `task-references-crate-safety`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the `References` arm of `resolve_pending_edges` + a unit test in the `tests` module)
- Modify: `crates/spur-graph/src/store/build.rs` (extend the `rebind_cross_file_edges` Calls guard to also cover `References`; `RESOLVER_VERSION` bump → v7)
- Regenerate (bless, if changed): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`
- Modify (only if an assertion legitimately changes): `crates/spur-graph/tests/extractor.rs`, `crates/spur-graph/tests/hof_references_edges.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] In the `References` arm of `resolve_pending_edges`, a singleton HOF-reference bind is emitted only when `function_singleton_safe(src_file, tgt_file)` is true (same-file or same-crate). When both file paths are known and it is false → leave the edge **unresolved** (`add_pending_edge(&edge, None)`, label kept). Preserve current behavior when file info is missing (only the *provably*-unsafe case drops). The `phantom_blocked_references` counter now corresponds to a genuine drop.
- [ ] `rebind_cross_file_edges` (`store/build.rs`) does NOT re-bind a cross-boundary `References` edge: extend the existing Calls guard so the `(method && !same_directory_path) || (function && !function_singleton_safe)` drop also applies when `edge.relation == RelationKind::References` (not just `Calls`). Leave the Imports path, `rebind_candidate_kinds`, range/kind guards, and all other relations UNCHANGED.
- [ ] A unit test (beside `singleton_function_call_respects_crate_safety`) proves: a cross-crate singleton HOF reference is unresolved; a same-crate / same-file singleton HOF reference still binds. (Drive through `build_facts` on a two-crate fixture, mirroring the existing function test.)
- [ ] An artifact-level assertion (in `tests/artifact_range_invariants.rs`, mirroring `artifact_rebind_preserves_cross_crate_singleton_function_unresolved`) proves a cross-crate singleton HOF reference stays unresolved AFTER `artifact_from_facts`.
- [ ] Goldens re-blessed if changed; diff is **drops-only on `references` edges**; no other relation changes, no target redirected. If anything else changes, STOP and emit `risk`.
- [ ] `RESOLVER_VERSION` bumped to a v7 value (e.g. `"2026-06-05-references-crate-safety-v7"`).
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; `gate_contract` + clippy `-D warnings` clean.
- [ ] **Report:** count of `references` singleton binds dropped (resolver) and the artifact-level cross-crate references count after rebind (expect ~0), which goldens changed, and v7 confirmation.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the `References` resolver arm + extending the rebind Calls guard to References + a unit test + an artifact test + `RESOLVER_VERSION` + re-blessed goldens. OUT: the `Calls` paths (already v6), the `scope_match` METHOD crate-guard (separate — needs design), import-aware recall recovery (T1.b.2-b — needs design), the resolver `file_for_node` O(n) fallback (leave as-is; the References arm already uses `file_by_id` directly), `function_singleton_safe` logic (reuse only), `method_scope_matches`, `same_directory_path`, `schema.rs`, the qualified/dyn paths, other relations, other crates.

**Implementation:**

- [ ] **Step 1: Failing test.** In the `tests` module of `tree_sitter.rs`, add a two-crate fixture where a bare singleton HOF reference (a function passed as a value, captured as a `References` edge) targets a function in a different crate; assert the resulting `references` edge is unresolved (`target_node_id = None`). Add a same-crate positive case that still binds. (Use `crates/a/...` vs `crates/b/...` paths, like `singleton_function_call_respects_crate_safety`.) Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib references` → expect FAIL.

- [ ] **Step 2: Gate the resolver References arm.** Rewrite the bind to honor the flag (the metering already computes it):

```rust
} else if edge.relation == RelationKind::References {
    if let Some(target) = singleton_symbols_by_label.get(&edge.target_name).copied() {
        if target != edge.source
            && matches!(
                node_kind_by_id.get(&target).copied(),
                Some(NodeKind::Function | NodeKind::Method)
            )
        {
            let provably_unsafe = matches!(
                (file_by_id.get(&edge.source).copied(), file_by_id.get(&target).copied()),
                (Some(src_file), Some(tgt_file)) if !function_singleton_safe(src_file, tgt_file)
            );
            if provably_unsafe {
                phantom_blocked_references += 1;
                self.add_pending_edge(&edge, None);
            } else {
                self.add_pending_edge(&edge, Some(target));
            }
        }
    }
}
```

  Run Step 1 again → expect PASS.

- [ ] **Step 2b: Extend the rebind guard to References.** In `rebind_cross_file_edges` (`store/build.rs`, the Calls guard added in v6), broaden the relation check so References is covered too:

```rust
} else if (edge.relation == RelationKind::Calls || edge.relation == RelationKind::References)
    && ((resolved.symbol_kind == "method"
        && !same_directory_path(&resolved.file_path, source_file_path))
        || (resolved.symbol_kind == "function"
            && !function_singleton_safe(source_file_path, &resolved.file_path)))
{
    edge.target_stable_symbol_id = None;
} else {
    // … unchanged (the Calls→function "singleton" stamp stays Calls-only) …
}
```

  Keep the trailing `if edge.relation == RelationKind::Calls && resolved.symbol_kind == "function"` singleton-stamp UNCHANGED (do not stamp References). Add the artifact-level test (Step from acceptance) and run `--test artifact_range_invariants` → expect PASS.

- [ ] **Step 3: Bump `RESOLVER_VERSION`** in `build.rs` (~line 28) `…-singleton-crate-safety-v6` → `"2026-06-05-references-crate-safety-v7"`.

- [ ] **Step 4: Re-bless goldens if changed (drops-only on `references`).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Diff must ONLY drop resolved targets on `references` edges (target → null, label kept). NO other relation changes. If anything else changed, STOP and emit `risk`.

- [ ] **Step 5: Broad gate + commit** (green except flaky `incremental_ingest`, do NOT run it):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test hof_references_edges --test artifact_range_invariants --test calls_range_resolution_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/artifact_range_invariants.rs crates/spur-graph/tests/fixtures/
# plus tests/extractor.rs / tests/hof_references_edges.rs if an assertion legitimately changed
git commit -m "fix(spur-graph): honor function_singleton_safe for cross-boundary HOF references"
```

  Report: resolver references-drop count, artifact-level cross-crate references count after rebind (expect ~0), which goldens changed, any assertion updated, and v7 confirmation.

## Self-Review
- **Coverage:** closes the parallel `References`/HOF phantom surface the code itself flagged as
  "measurement-only," using the exact resolver+rebind pattern validated by v6.
- **Placeholder scan:** concrete arm rewrite + concrete rebind extension + concrete unit/artifact tests;
  golden re-bless budgeted/conditional.
- **Type consistency:** `function_singleton_safe` (now `pub(crate)`), `file_by_id`, `same_directory_path`,
  `phantom_blocked_references` all already present in the same functions.
- **DAG:** single task.
- **Risk:** resolution-semantics change bounded to the References arm + one rebind relation-check
  broadening; Calls/method/range guards untouched; diff-is-references-drops-only golden check + `risk`
  off-ramp. Recall impact is small (~110 references) and honest (labels retained). The big recall-recovery
  and method-gating work is explicitly deferred to a design-first follow-up.
