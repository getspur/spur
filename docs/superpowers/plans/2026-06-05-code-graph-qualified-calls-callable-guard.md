# Guard qualified `calls` resolution to callable targets Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Grounding (live artifact `7e0e467b`, post rebind-guard rebuild):** after the resolver + rebind fixes,
`extends`/`implements` out-of-range = 0 and `calls`→non-callable dropped 4790 → **49**. The residual 49
are ALL `calls→field` with `bind_method="fqn"` — i.e. resolved by the **qualified-name path**, a third
resolution path that the singleton guard (P1a) and the rebind guard did not cover.

**The bug (exact):** in `resolve_pending_edges` (`crates/spur-graph/src/extract/tree_sitter.rs`,
the `else if edge.relation == RelationKind::Calls` qualified branch, ~lines 587-608), the single-match
arm reclassifies `Struct`/`EnumVariant`/`Class` → `Constructs` but its `else` binds **every other kind**
with `bind_method="fqn"` and no callable check:

```rust
[target] if *target != edge.source => {
    if matches!(kind, Struct | EnumVariant | Class) {
        self.add_pending_edge_as(&edge, Some(*target), RelationKind::Constructs, None);
    } else {
        self.add_pending_edge_with_bind_method(&edge, Some(*target), Some("fqn")); // ← binds field/module/etc.
    }
}
```

So a qualified call that FQN-matches a `field` (or `module`/`section`/`constant`/`enum`/`macro`) is
bound as a `calls` edge.

**Goal:** Make the qualified `calls` bind callable-only: keep the `"fqn"` bind ONLY when the target is
`Function`/`Method` (the `Constructs` reclassification stays); any other kind → leave unresolved.

📌 **Golden artifacts may change** (corpus qualified calls→non-callable drop to unresolved). Re-blessing
the 4 corpus goldens is **in scope and budgeted** (likely only a subset changes; possibly none).

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task qcalls-guard: callable-guard the qualified calls bind

**Task ID:** `task-qualified-calls-guard`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the qualified `Calls` `[target]` arm only)
- Modify: `crates/spur-graph/src/store/build.rs` (`RESOLVER_VERSION` bump → v5)
- Modify: `crates/spur-graph/tests/calls_range_resolution_edges.rs` (add a qualified-path case)
- Regenerate (bless, only if changed): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`

**Depends on:** none

**Acceptance Criteria:**
- [ ] No `calls` edge with `bind_method="fqn"` resolves to a target whose kind ∉ {Function, Method}.
      (Struct/EnumVariant/Class continue to reclassify to `Constructs`.)
- [ ] A qualified call that FQN-matches a non-callable (e.g. a `field`) is left unresolved
      (`target_node_id = None`, label retained) instead of bound.
- [ ] A genuine qualified call to a `Function`/`Method` still resolves with `bind_method="fqn"`.
- [ ] Goldens re-blessed if they changed (diff is drops-only on `calls`); full `-p spur-graph` suite
      green except the flaky `incremental_ingest`; `gate_contract` + clippy `-D warnings` clean.
- [ ] `RESOLVER_VERSION` bumped to a v5 value (e.g. `"2026-06-05-qualified-calls-callable-v5"`).

**Suggested Worker:** codex.

**Scope Boundary:** IN: the one qualified-`Calls` arm + `RESOLVER_VERSION` + a test + re-blessed
goldens. OUT: the rebind pass (`store/build.rs::rebind_cross_file_edges` — already guarded), the
singleton/relational paths, `schema.rs`, other relations, other crates. Do NOT change the `Constructs`
reclassification or the `len() > 1` / qualified-miss arms.

**Implementation:**

- [ ] **Step 1: Failing test.** In `crates/spur-graph/tests/calls_range_resolution_edges.rs`, add a case
  that triggers the QUALIFIED path (not the singleton fallback): a qualified call expression whose
  qualified target FQN-matches a `field`. If a minimal Rust fixture is hard to construct, assert the
  general invariant over `build_facts` output — no `calls` edge with `bind_method.as_deref()==Some("fqn")`
  has a target whose `NodeKind` ∉ {Function, Method} — using a fixture exercising qualified calls. Run
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test calls_range_resolution_edges` → expect FAIL
  (or, if a minimal repro can't be crafted, note that and rely on the invariant assertion + golden).

- [ ] **Step 2: Add the callable guard.** Change the qualified `Calls` `[target]` arm so the non-construct
  branch only binds callable targets:

```rust
[target] if *target != edge.source => {
    let kind = node_kind_by_id.get(target).copied();
    if matches!(kind, Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)) {
        self.add_pending_edge_as(&edge, Some(*target), RelationKind::Constructs, None);
    } else if matches!(kind, Some(NodeKind::Function | NodeKind::Method)) {
        self.add_pending_edge_with_bind_method(&edge, Some(*target), Some("fqn"));
    } else {
        // qualified match to a non-callable (field/module/section/...) is a misresolution
        self.add_pending_edge(&edge, None);
    }
}
```

  Run Step 1 again → expect PASS.

- [ ] **Step 3: Bump `RESOLVER_VERSION`** in `build.rs` (~line 26) to `"2026-06-05-qualified-calls-callable-v5"`.

- [ ] **Step 4: Re-bless goldens if changed.**

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  The diff (if any) must ONLY drop resolved targets on `calls` edges whose target is non-callable
  (target → null, label kept). NO other relation changes, no target redirected to a different node. If
  anything else changed, STOP and emit `risk`.

- [ ] **Step 5: Broad gate + commit** (green except flaky `incremental_ingest`, do NOT run it):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test calls_range_resolution_edges --test extractor \
  --test resolver --test range_resolution_edges --test constructs_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/calls_range_resolution_edges.rs crates/spur-graph/tests/fixtures/
git commit -m "fix(spur-graph): callable-guard qualified calls resolution"
```

  Report: the resolved→unresolved `calls` delta, which goldens changed (if any), and confirmation
  `RESOLVER_VERSION` bumped to v5.

## Self-Review
- **Coverage:** closes the third (qualified-name) `calls` path that bound non-callable targets, the last
  residual after the resolver + rebind guards (49 `calls→field` "fqn" → 0).
- **Placeholder scan:** concrete arm + concrete invariant test; golden re-bless budgeted/conditional.
- **Type consistency:** `NodeKind::{Function,Method,Struct,EnumVariant,Class}`, `add_pending_edge*`,
  `node_kind_by_id` all already used in the same match.
- **DAG:** single task.
- **Risk:** one match arm; `Constructs` + ambiguous + qualified-miss arms untouched; diff-is-calls-drops-only
  golden check + `risk` off-ramp.
