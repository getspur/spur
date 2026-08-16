# Design Rule Solver Catalog Implementation Plan

**Spec:** `docs/superpowers/specs/2026-08-16-design-rule-solver.md`
**Issue:** `bd-2bx`
**Method:** TDD, one RED -> GREEN -> REFACTOR cycle per behavior
**Post-implementation ordering proof:** `sol_21eddfc43e2147a3`

## Task 1: Contract and module boundary

**Files:**
- Modify `crates/spur-solver/src/lib.rs`
- Create `crates/spur-solver/src/rules/mod.rs`
- Create `crates/spur-solver/src/rules/families/mod.rs`

**Acceptance:** `spur-solver` exposes a multi-family rule subsystem while the
generic `SolverService` execution path remains unchanged.

## Task 2: Registry types and validation

**RED:** Tests require deterministic profile listing, exact rule lookup,
selector exclusivity, unique IDs, and valid profile membership.

**GREEN:** Add versioned registry types and validation with no solver calls.

## Task 3: Seed geometric rules

**RED:** Tests request complete details for containment, non-overlap, and aspect
ratio, including authority, examples, LLM encoding, and solver encoding.

**GREEN:** Add the `design` family and its `geometric_integrity` static catalog entries.

## Task 4: `solve_rule_spec` vertical slice

**RED:** MCP tests require the schema, empty listing, each selector mode,
progressive `include`, unknown-selector errors, and catalog-only dispatch.

**GREEN:** Add `solve_rule_spec` to the shared `SolverMcpModule` and preserve
brain/worker/catalog registry composition. Keep the guide independent from live
Z3 state.

## Task 5: Scene IR

**RED:** Tests reject invalid dimensions, missing parents, missing binding
subjects, duplicate unknown paths, and invalid ranges.

**GREEN:** Add typed scene and unknown request types with deterministic
validation. Rule bindings are compiled in the following task.

## Task 6: B-prime compiler

**RED:** Tests assert the exact predicate shape for containment, non-overlap,
and aspect ratio, plus assert-negation for verification.

**GREEN:** Compile to public `spur_solver::types` without raw SMT.

## Task 7: `solve_rules`

**RED:** Tests require verify/synthesize request parsing and preservation of all
solver statuses. Real-Z3 integration covers `pass`, `fail`, `solution`, and
`infeasible`; a pure status table covers inconclusive states.

**GREEN:** Delegate compiled requests to the shared `SolverService`; add domain
outcome without changing the raw status, model, core, persistence, or SMT echo.

## Task 8: Integration and verification

**RED:** Core registry tests require both new tools in brain, worker, and
catalog listings and callable through live registries.

**GREEN:** Finish composition, docs, and symbolic ordering verification.

**Verification:**

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-solver
scripts/spur-cargo test -p spur-core --test mcp_signals_catalog
scripts/spur-cargo check -p spur-solver -p spur-core
```

Re-run the symbolic task-order constraints after implementation and persist the
final solve artifact. Never treat `unknown` or `timeout` as completion proof.
