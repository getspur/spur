# Honor `function_singleton_safe` — stop emitting cross-boundary phantom call binds (Tier-1 T1.b.2-a) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** graph-grounded + structural analysis (code-explore + spur-analyst, artifact `541a44a1…`, indexed HEAD `483ca0ad`).

**The bug (exact, grounded):** in `resolve_singleton_bare_target`
(`crates/spur-graph/src/extract/tree_sitter.rs`, the `Some(NodeKind::Function) if edge.relation == RelationKind::Calls` arm, ~lines 864-875), the resolver computes the safety signal and then **ignores it**:

```rust
Some(NodeKind::Function) if edge.relation == RelationKind::Calls => {
    if let (Some(src_file), Some(tgt_file)) = (
        indexes.file_by_id.get(&edge.source).copied(),
        indexes.file_by_id.get(&target).copied(),
    ) {
        if !function_singleton_safe(src_file, tgt_file) {
            *phantom_blocked_calls += 1;          // ← counted…
        }
    }
    builder.add_pending_edge_with_bind_method(edge, Some(target), Some("singleton")); // …but bound ANYWAY
}
```

`function_singleton_safe` (`:1105`) returns `true` only for same-file or same-crate (`path_scope` = `crates/<name>`); cross-crate and non-crate paths return `false`. The variable is named `phantom_blocked_calls` but **nothing is blocked** — every flagged-unsafe bind is still emitted.

**Why these are misresolutions (the closed-world bug):** the graph has no model of symbols outside the worktree (stdlib, prelude, third-party). When a bare callee name has exactly ONE workspace definition, it binds there even when the real callee is external. Structural evidence (self-graph): **620** `calls` edges with `bind_method="singleton"` to a `function` cross a crate/non-crate boundary. The top sinks are unmistakably stdlib names with one coincidental workspace twin — `eq_ignore_ascii_case` (×24 from 8 crates), `split_once` (×20/9), `skip` (×22/9), `canonicalize` (×23/5), `first` (×63/8), `env` (×32/7). Every `.split_once()` in the workspace phantom-binds to the lone same-named symbol. Method binds (`scope_match`, 1,188 cross-crate) are the same disease and are **out of scope here** (softer; a name-scoped bandage exists) — a follow-up.

**Goal:** Honor the signal already computed. When the singleton FUNCTION bind is provably unsafe (`function_singleton_safe` is false on known file paths), **leave the edge unresolved** (`add_pending_edge(edge, None)` — label kept, `target_node_id = None`) instead of asserting a cross-boundary guess. Tier-0/Tier-1 honesty principle: **an unresolved label beats a confident lie.**

**Accepted tradeoff (must be measured, not hidden):** a minority of the 620 are *genuine* cross-crate workspace calls (e.g. a spur-cli test calling `spur-graph::build_facts`, `artifact_from_facts`, `write_artifact_parquet`). Honoring the flag drops these to unresolved too. That recall cost is **accepted** for this step (precision floor first) and is **recoverable** by the queued follow-up **T1.b.2-b** (import-aware binding: keep a cross-crate bind only when the caller's file has a backing `imports` edge — needs threading an import index into `PendingResolutionIndexes`, which today has none). The worker MUST report the list of genuine functions losing cross-crate inbound so the reviewer can judge the cost before merge.

📌 **Golden artifacts WILL change.** Corpus fixtures store non-`crates/`-prefixed paths (e.g. `src/lib.rs`), so `path_scope` is `None` → cross-FILE singleton function calls in the corpora are "non-crate unsafe" and now drop to unresolved. This is the gate working (the existing `function_singleton_safe_non_crate_path` unit test documents this path). Re-blessing the 4 corpus goldens is **in scope and budgeted**; the diff MUST be **drops-only on `calls` edges** (target → null, label kept) — no other relation changes, no target redirected.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task singleton-crate-safety: gate the singleton function bind on `function_singleton_safe`

**Task ID:** `task-singleton-crate-safety`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the one Function arm of `resolve_singleton_bare_target` + a unit test in the existing `tests` module beside `function_singleton_safe_*`)
- Modify: `crates/spur-graph/src/store/build.rs` (`RESOLVER_VERSION` bump → v6)
- Regenerate (bless, expected): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`
- Modify (only if an assertion legitimately changes): `crates/spur-graph/tests/extractor.rs`, `crates/spur-graph/tests/resolver.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] In `resolve_singleton_bare_target`, a singleton FUNCTION `calls` bind is emitted **only** when `function_singleton_safe(src_file, tgt_file)` is true (same-file or same-crate). When both file paths are known and it is false → the edge is left **unresolved** (`add_pending_edge(edge, None)`, label retained). Same-file and same-crate singleton function binds are **unchanged** (still `bind_method="singleton"`).
- [ ] Method/relational/constructs/python-implements branches of `resolve_singleton_bare_target` are **untouched** (this task is the Function arm only).
- [ ] A unit test (beside the existing `function_singleton_safe_*` tests) asserts: a cross-crate singleton function call resolves to `None` (unresolved); a same-crate / same-file singleton function call still binds with `bind_method="singleton"`. (Drive it through `resolve_singleton_bare_target` or `build_facts`, whichever is the lighter faithful harness.)
- [ ] Corpus goldens re-blessed; the diff is **drops-only on `calls` edges** to functions whose bind was non-crate/cross-crate (target → null, label kept). NO other relation changes; if anything else changes, STOP and emit `risk`.
- [ ] `RESOLVER_VERSION` bumped to a v6 value (e.g. `"2026-06-05-singleton-crate-safety-v6"`).
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; `gate_contract` + clippy `-D warnings` clean.
- [ ] **Report (for reviewer):** the count of `calls` function singleton binds dropped, AND the list of *genuine workspace* functions that lost cross-crate inbound (distinguish from stdlib-name phantoms) so the recall cost is visible.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the one Function arm + its unit test + `RESOLVER_VERSION` + re-blessed goldens. OUT: the method `scope_match` path (the 1,188 cross-crate method binds — separate follow-up T1.b.2-b), threading an import index into `PendingResolutionIndexes` (that IS T1.b.2-b), `schema.rs`, the rebind/qualified paths, other relations, other crates. Do NOT change `function_singleton_safe` itself, `method_scope_matches`, the constructs reclassification, or the Method arm.

**Implementation:**

- [ ] **Step 1: Failing test.** Add a unit test in the `tests` module of `tree_sitter.rs` (beside `function_singleton_safe_cross_crate_blocks`). Construct/extract a case where a bare function call's only same-named definition is in a different crate path (or a non-`crates/` path) and assert the resulting `calls` edge is unresolved (`target_node_id = None`); add a same-crate positive case that still binds `"singleton"`. If a unit-level harness for `resolve_singleton_bare_target` is awkward, assert the invariant over `build_facts` output on a small two-file fixture with `crates/a/...` vs `crates/b/...` style paths. Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib singleton` → expect FAIL.

- [ ] **Step 2: Gate the bind.** Change ONLY the Function arm so the unsafe case is honest:

```rust
Some(NodeKind::Function) if edge.relation == RelationKind::Calls => {
    let provably_unsafe = matches!(
        (
            indexes.file_by_id.get(&edge.source).copied(),
            indexes.file_by_id.get(&target).copied(),
        ),
        (Some(src_file), Some(tgt_file)) if !function_singleton_safe(src_file, tgt_file)
    );
    if provably_unsafe {
        *phantom_blocked_calls += 1;
        builder.add_pending_edge(edge, None); // honest: don't assert a cross-boundary guess
    } else {
        builder.add_pending_edge_with_bind_method(edge, Some(target), Some("singleton"));
    }
}
```

  (Preserve current behavior when file info is missing — only the *provably* unsafe case drops. The `phantom_blocked_calls` increment now genuinely corresponds to a dropped edge.) Run Step 1 again → expect PASS.

- [ ] **Step 3: Bump `RESOLVER_VERSION`** in `build.rs` (~line 26) `…-qualified-calls-callable-v5` → `"2026-06-05-singleton-crate-safety-v6"`.

- [ ] **Step 4: Re-bless goldens (expected drops-only on `calls`).**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Sanity-check the golden diff: ONLY `calls` edges drop their resolved target (→ null, label kept). NO `imports`/`contains`/`defines`/`constructs`/`extends`/`implements` change, no target redirected to a different node. If anything else changed, STOP and emit `risk`.

- [ ] **Step 5: Broad gate + commit** (green except flaky `incremental_ingest`, do NOT run it):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor --test resolver \
  --test calls_range_resolution_edges --test range_resolution_edges --test artifact_range_invariants \
  --test constructs_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/fixtures/
# plus tests/extractor.rs / tests/resolver.rs if an assertion legitimately changed
git commit -m "fix(spur-graph): honor function_singleton_safe to drop cross-boundary phantom calls"
```

  Report: the dropped `calls`-singleton count, the genuine-vs-phantom split of dropped target names, which goldens changed, any assertion updated, and confirmation `RESOLVER_VERSION` bumped to v6.

## Self-Review
- **Coverage:** closes the highest-value Tier-1 binding-precision leak — the resolver already computes `function_singleton_safe` and discards it; this wires it. 620 cross-boundary phantom function binds → unresolved.
- **Placeholder scan:** concrete arm rewrite + concrete unit test; golden re-bless budgeted; recall cost explicitly measured and reported.
- **Type consistency:** `function_singleton_safe(&str,&str)`, `file_by_id: HashMap<NodeId,&str>`, `add_pending_edge`/`add_pending_edge_with_bind_method`, `phantom_blocked_calls` all already in the same function.
- **DAG:** single task.
- **Risk:** resolution-semantics change bounded to ONE match arm; Method/constructs/relational arms untouched; diff-is-`calls`-drops-only golden check + `risk` off-ramp. Recall tradeoff is intentional, measured, and recoverable by the queued import-aware follow-up (T1.b.2-b).
