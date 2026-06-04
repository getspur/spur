# Ontology Tier-0 P1b — Relation realization contract gate Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-spec-live-evidence.ipynb` (§6 Piece 5b, P1b).

**Grounding (read the gate):** the contract test in `crates/spur-graph/src/extract/languages.rs`
(`~:1143`) enforces only the **definition** vocabulary — every `@definition.*` capture has a
`definition_kind_map` key, `tags` queries are non-empty, every map key appears as a capture, every
`NodeKind` has a `symbol_kind()`. It asserts **nothing** about which **relations (predicates)** each
language realizes. The "Relation Coverage Matrix" in `crates/spur-graph/queries/README.md` is therefore
a human-only document — a language could silently stop realizing `implements`/`calls`/etc. and no test
would fail.

**Goal:** Add a **relation-coverage contract test** that pins, per language family, the set of
predicates realized by its `spur-edges.scm` captures (as dispatched by `emit_edges`), and fails if the
realized set drifts from an explicit expected table mirroring the README matrix. `TODO` cells in the
matrix become explicit allow-listed gaps in the test, so an *intended* gap is declared and an
*accidental* regression is caught.

**Scope:** a TEST + a small expected-table constant ONLY. No change to extraction, resolution, queries,
or schema. This is additive governance, behavior-neutral.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task p1b: relation realization contract gate

**Task ID:** `task-p1b-relation-gate`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/languages.rs` (new `#[test]` + an expected
  `language → {realized RelationKind}` table; reuse the existing test module + registry/config access)
- Modify (doc parity only, if needed): `crates/spur-graph/queries/README.md` (no matrix value changes
  expected; only touch if the test surfaces a real mismatch — if so STOP and emit `risk` first)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A test derives, for each configured language, the set of `RelationKind`s its `spur-edges.scm`
      captures realize — by mapping each edge capture name through the SAME dispatch `emit_edges` uses
      (`"import"→Imports`, `"call"/"jsx_call"/"macro_call"→Calls`, `"implements"→Implements`,
      `"extends"→Extends`, `"reference.name"→References`, markdown `"link"→Links`, …). Do not duplicate
      the dispatch by hand if it can be referenced; if it must be mirrored, add a comment tying the two
      together so they cannot silently diverge.
- [ ] The test asserts the realized set equals an explicit expected table that mirrors the README
      matrix (Rust/Python/TypeScript/Tsx/Cpp/Markdown). Each README `TODO` is an allow-listed expected
      gap (documented in a comment), NOT a silent omission.
- [ ] Removing an edge capture from any `spur-edges.scm`, or adding a new language without declaring its
      predicates, fails the test with a clear message.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib` green (incl. the new test) except the
      flaky `incremental_ingest`; clippy `-D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the test + expected table in `languages.rs`. OUT: changing any `.scm`, the
`emit_edges` dispatch, `RelationKind`, the resolver, the definition gate, other crates. If the realized
set does NOT match the README matrix, that is a real finding → STOP, emit `risk` with the mismatch; do
NOT "fix" it by editing the matrix or the queries to make the test pass.

**Implementation:**

- [ ] **Step 1: Map the dispatch.** Read `emit_edges` (`languages.rs:~448-572`) and record the exact
  capture-name → `RelationKind` mapping it performs. Read each `queries/<lang>/spur-edges.scm` and
  enumerate its edge capture names.

- [ ] **Step 2: Failing test.** Add `relation_coverage_matches_declared_contract` to the `languages.rs`
  test module: build `actual: BTreeMap<Language, BTreeSet<RelationKind>>` from each config's edge
  captures via the Step-1 mapping; define `expected` mirroring the README matrix with `TODO`s as
  documented allow-listed gaps; `assert_eq!(actual, expected)` with a diff-friendly message. Run it;
  if it fails because the matrix and reality already disagree, STOP and emit `risk` (do not paper over).

- [ ] **Step 3: Make it pass honestly.** If the only failures are because the expected table was
  mis-transcribed from the README, correct the *expected table* to match BOTH reality and the README.
  (Reality and README should already agree per the spec.)

- [ ] **Step 4: Gate + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib relation_coverage
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/languages.rs
git commit -m "test(spur-graph): enforce per-language relation realization contract"
```

  Report: the realized-predicate set per language, and confirm it matches the README matrix exactly
  (including which TODOs are allow-listed).

## Self-Review
- **Spec coverage:** §6 Piece 5b P1b — the relation matrix becomes enforced, not advisory.
- **Placeholder scan:** concrete test + expected table; explicit `risk` off-ramp if reality≠matrix.
- **Type consistency:** `Language`, `RelationKind`, the registry/config accessors already exist in the
  same module and test scope.
- **DAG:** single task.
- **Risk:** behavior-neutral (test-only); the only failure mode is discovering a real drift, which is
  routed to `risk` rather than silently reconciled.
