# Boolean Equality Semantics Implementation Plan

> **For SPUR orchestrator:** This plan was submitted with `persist_as_epic`
> semantics. Each task is a beads issue with plan and task lineage.

**Source spec:** `docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md`
**Design and execution epic:** `bd-3ch2p`
**Plan ID:** `1654ac8f-47ee-4723-b3b2-4a5422fd0174`

**Goal:** Make `eq` and `ne` accept any two Boolean-sorted expressions while
preserving existing cross-sort and enum-domain validation.

**Architecture:** Keep the typed B-prime AST and encoder unchanged. Broaden the
validator's compatible-equality matrix from origin-sensitive Boolean pairs to
`Bool(_)/Bool(_)`, update the normative documentation, then independently
review the integrated result.

**Tech Stack:** Rust 2021, serde-backed B-prime AST, Z3 subprocess,
`scripts/spur-cargo`, Markdown skills and specifications.

---

### Task 1: Implement same-sort Boolean equality

**Task ID:** `rust-bool-equality`
**Beads issue:** `bd-1zxvp`

**Files:**

- Modify and test: `crates/spur-solver/src/types.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] A failing regression-test commit covers variable, literal, and compound
      Boolean expressions for both `eq` and `ne`.
- [ ] A separate fix commit accepts every `Bool(_)/Bool(_)` pairing.
- [ ] `Bool/Int`, other cross-sort comparisons, and mismatched enum domains
      remain invalid.
- [ ] `scripts/spur-cargo test -p spur-solver` passes.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-solver -- -D warnings`
      passes.

**Suggested Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`,
effort `max`, as explicitly selected by the user.

**Scope Boundary:**

- IN scope: Boolean equality validation and its unit tests in `types.rs`.
- OUT of scope: encoder rewrites, AST redesign, solver process behavior,
  persistence, and generated skill projections.
- If another file is required, emit `scope_drift` before changing it.

**Implementation:**

1. Add a focused test that expects `Ok(())` for:
   `var/var`, `literal/literal`, `compound/literal`, and `compound/compound`
   Boolean `eq`/`ne` expressions.
2. Run the focused test through `scripts/spur-cargo` and record the expected
   `TypeMismatch` failure.
3. Commit the failing test with a `test(spur-solver): ...` subject.
4. Change `equality_is_valid` to accept
   `(ExpressionSort::Bool(_), ExpressionSort::Bool(_))`.
5. Retain or add explicit negative coverage for `Bool/Int` and mismatched enum
   domains.
6. Run formatting, the complete `spur-solver` test suite, and remote clippy.
7. Commit the implementation with a `fix(spur-solver): ...` subject.

### Task 2: Align the solver specification and skill

**Task ID:** `document-bool-equality`
**Beads issue:** `bd-3e1v5`

**Files:**

- Modify:
  `docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md`
- Modify: `assets/skills/solve/SKILL.md`

**Depends on:** none

**Acceptance Criteria:**

- [ ] The specification states that `eq` and `ne` accept compatible same-sort
      operands, including every Boolean expression origin.
- [ ] Integer ordering and same-domain enum restrictions remain explicit.
- [ ] The solve skill tells agents not to manually expand Boolean equivalence.
- [ ] No generated runtime or client skill projection is modified.
- [ ] The two authoritative documents contain no contradictory equality rule.

**Suggested Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`,
effort `max`, as explicitly selected by the user.

**Scope Boundary:**

- IN scope: the existing normative solver specification and source solve skill.
- OUT of scope: generated `.spur`, `.codex`, `.claude`, or other client
  projections.
- If contradictory guidance exists outside these files, emit `scope_drift`
  instead of broadening the edit.

**Implementation:**

1. Replace the specification's Boolean-variable/literal-only rule with the
   compatible equality matrix.
2. Preserve that `lt`, `le`, `gt`, and `ge` are integer-only and enum equality
   requires the same domain.
3. Add the same-sort rule and direct Boolean-equality guidance to the solve
   skill.
4. Scan both files for stale or contradictory equality language.
5. Commit with a `docs(spur-solver): ...` subject.

### Task 3: Review and verify the integrated change

**Task ID:** `review-bool-equality`
**Beads issue:** `bd-9wcnk`

**Files:**

- Review and correct if necessary: `crates/spur-solver/src/types.rs`
- Review and correct if necessary:
  `docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md`
- Review and correct if necessary: `assets/skills/solve/SKILL.md`

**Depends on:** `rust-bool-equality`, `document-bool-equality`

**Acceptance Criteria:**

- [ ] The implementation exactly enforces `Int/Int`, `Bool(_)/Bool(_)`, and
      same-domain `Enum/Enum` compatibility.
- [ ] Regression tests cover `eq` and `ne` across all Boolean origins.
- [ ] Cross-sort and mismatched enum comparisons still fail validation.
- [ ] Documentation and implementation agree.
- [ ] Formatting, `spur-solver` tests, and remote clippy pass.
- [ ] Findings are reported by severity; corrective edits are minimal.

**Suggested Worker:** `codex`, profile `code-reviewer`, model `gpt-5.6-sol`,
effort `max`, as explicitly selected by the user.

**Scope Boundary:**

- IN scope: review and necessary corrections in the three predecessor files.
- OUT of scope: unrelated refactors, generated skill projections, and other
  solver subsystems.
- If a broader defect is found, emit `risk` or `scope_drift` instead of
  expanding the patch.

**Implementation:**

1. Inspect both predecessor diffs and verify the equality compatibility matrix.
2. Check test coverage for positive Boolean-origin pairs and negative
   cross-sort/domain cases.
3. Run:
   `scripts/spur-cargo fmt --all -- --check`,
   `scripts/spur-cargo test -p spur-solver`, and
   `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-solver -- -D warnings`.
4. Make and commit only corrections required for correctness.
5. Report findings, exact verification evidence, and any corrective commit.

## Dependency DAG

```text
rust-bool-equality ────────┐
                           ├──> review-bool-equality
document-bool-equality ────┘
```

The two root tasks have disjoint write sets and can execute in parallel. The
review task waits for both overlays so it validates the integrated semantics.
