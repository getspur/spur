# Language-family gate for relational (`extends`/`implements`) resolution (v12) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source:** cross-language evaluation of the v11 resolver against an external Python+JS repo
(`anywidget`, 76 files). The eval confirmed binding precision generalizes (zero cross-language binds),
but surfaced that **`extends`/`implements` is the only resolution arm without a language-family
gate**. Calls (`function_singleton_safe`/v9), `method_crate_singleton` (v10), and `constructs`
(v11) all gate on language family; `relational_symbol_candidates` does not.

**The gap (exact, grounded):** `relational_symbol_candidates`
(`crates/spur-graph/src/extract/tree_sitter.rs`) collects candidates for an `extends`/`implements`
edge filtered to `{Trait, Interface, Class}` but **not by language**. When a type name is defined in
more than one language, the candidate set has >1 entry → the relational arm treats it as ambiguous
and drops the edge. Live evidence on anywidget: **26 `class XWidget(AnyWidget)` edges (all Python
source) left unresolved** because `AnyWidget` exists as a Python `class` AND a TS `interface` →
2 candidates → ambiguous. A call/extends never crosses a language boundary, so filtering candidates
to the source's language family reduces `{py class, ts interface}` → `{py class}` → unique →
**resolves to the correct in-repo base class**.

**Why this is strictly an improvement (not a tightening that risks recall):** the language filter
only *removes* candidates of a different language family. It can therefore turn an ambiguous
(>1 candidate) set into a unique one (**recall ↑**) and can **never** create a cross-language bind
(**precision-safe**). It is the exact same predicate already proven three times (v9/v10/v11).

**Why it matters even though SPUR barely moves:** relational is the last un-gated resolution arm; on
every polyglot graph this asymmetry silently drops in-repo inheritance. Closing it makes the resolver
**uniformly language-agnostic**. SPUR's own corpora are single-language, so expect **zero/near-zero
golden churn** — if any SPUR golden changes, it is a *recovered* (ambiguous→unique) or a *dropped
cross-language phantom*, both of which are correct; inspect and confirm.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

**Out of scope:** the JavaScript-extractor coverage gap (separate plan, R1), the closed-world
calls/imports tail (Tier-2), receiver-type discrimination, and any non-relational arm.

---

### Task relational-language-gate: filter relational candidates to the source language family

**Task ID:** `task-relational-language-gate`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — add a source-language-family filter to
  `relational_symbol_candidates` (reusing the existing `file_path_for_node` + `language_family`
  helpers added in v11); add unit + behavioral tests.
- Modify: `crates/spur-graph/src/store/build.rs` — `RESOLVER_VERSION` bump → v12.
- Regenerate (bless — expect none/near-none): `crates/spur-graph/tests/fixtures/*/expected_graph_index.json`.

**Depends on:** none

**Acceptance Criteria:**
- [ ] `relational_symbol_candidates` filters its candidate list to nodes whose file shares the
      **same language family** as the source node's file (`language_family(src) == language_family(tgt)`,
      both `Some(_)`), reusing `file_path_for_node` + `language_family`. Same-file is trivially same
      family. When the source file's family is `None` (unknown extension), keep current behavior
      (no language filtering) so unknown-language extraction is not regressed.
- [ ] After filtering: exactly one candidate → resolve via `add_pending_relational_edge` as today;
      >1 → ambiguous unresolved as today; 0 → unresolved as today. (The arm body in
      `resolve_bare_pending_edge` is unchanged; only the candidate set is narrowed.)
- [ ] **Unit test** `relational_candidates_excludes_cross_language`: a `extends` edge from a Python
      class whose label matches BOTH a Python `class` and a TS `interface` → candidates contain only
      the Python class.
- [ ] **Behavioral `build_facts` test** `relational_language_gate_resolves_in_repo_base_class`
      (the AnyWidget pattern): a Python `class Child` extending `Base`, where `Base` exists as a
      Python `class` in one file AND a TS `interface`/`class` of the same name in another; assert the
      `extends` edge resolves to the **Python** `Base` (today it is dropped as ambiguous).
- [ ] No change to `relational_target_kinds`, `callable`/`constructs`/`import` candidate functions,
      `function_singleton_safe`, the per-language builtin lists, `path_scope`/`path_crate`, or
      `schema.rs`.
- [ ] Goldens re-blessed; **expect none/near-none** (single-language corpora). Any change must be a
      relational edge that is newly resolved (ambiguous→unique) or newly dropped (a cross-language
      phantom); if any NON-relational edge changes, STOP and emit `risk`.
- [ ] `RESOLVER_VERSION` bumped to `"2026-06-05-relational-language-gate-v12"`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** new tests pass; golden status (expect none); v12 confirmation. (Live effect —
      ≥26 recovered in-repo inheritance edges on polyglot repos — is verified by the brain against
      the anywidget graph.)

**Suggested Worker:** codex.

**Scope Boundary:** IN: the language-family filter in `relational_symbol_candidates` +
`RESOLVER_VERSION` + unit/behavioral tests + conditional bless. OUT: the JS extractor, the
relational arm body, other candidate functions, other relations, other crates, `schema.rs`.

**Implementation:**

- [ ] **Step 1: Failing tests.** Add the unit + behavioral tests above. Run
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib relational_language_gate` → expect the
  behavioral test to FAIL (today the cross-language collision drops the edge).

- [ ] **Step 2: Add the filter** in `relational_symbol_candidates`: after the existing
  kind filter, additionally retain only candidates whose file language family equals the source
  file's family (skip the filter when the source family is `None`). Re-run Step 1 → PASS.

- [ ] **Step 3: Bump `RESOLVER_VERSION`** (`build.rs:29`) → `"2026-06-05-relational-language-gate-v12"`.

- [ ] **Step 4: Bless (expect none).**
```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

- [ ] **Step 5: Broad gate + commit:**
```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/src/store/build.rs \
        crates/spur-graph/tests/fixtures/
git commit -m "fix(spur-graph): language-gate relational candidate resolution (extends/implements)"
```

## Self-Review
- **Coverage:** closes the last un-gated resolution arm; recovers in-repo inheritance lost to
  cross-language type-name collisions on polyglot graphs; mirrors v9/v11.
- **Risk:** strictly narrows candidates (recall ↑, never adds cross-language binds); single-language
  corpora ⇒ zero/near-zero churn; reuses helpers added in v11.
- **DAG:** single task.
