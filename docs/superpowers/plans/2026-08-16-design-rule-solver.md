# Design Rule Solver Catalog Implementation Plan

**Spec:** `docs/superpowers/specs/2026-08-16-design-rule-solver.md`
**Issue:** `bd-2bx`
**Method:** TDD, one RED -> GREEN -> REFACTOR cycle per behavior
**Post-implementation ordering proof:** `sol_21eddfc43e2147a3`
**Axis-capacity SOLVE PRE:** `sol_2367ba362d8a462e`, `sol_168998e1847e448e`
**Axis-capacity SOLVE POST:** `sol_37ec96b05dea413c`, `sol_49ed76c5de1e4e95`

## Task 1: Solver and model contract

**RED:** Tests require `verify` to reject unknowns and incomplete facts, and
require `synthesize` to accept only explicitly bounded unknowns. Status tests
require `verify/sat -> pass` and `verify/unsat -> fail` without collapsing
`unknown` or `timeout`.

**GREEN:** Tighten the shared mode and outcome contract while leaving the
generic `SolverService` unchanged.

## Task 2: Identity-preserving compiled rule IR

**RED:** Compiler tests require every selected binding to retain its stable
rule ID, binding index, predicate, and required variables.

**GREEN:** Compile family bindings to `CompiledRule` values, then lower them to
the aggregate B-prime request without erasing identity.

## Task 3: Verification and synthesis executor

**RED:** Real-Z3 tests require complete valid models to pass, invalid models to
fail with exact per-rule attribution, bounded synthesis to return a valid model,
and infeasible synthesis to remain `unsat`.

**GREEN:** Execute the aggregate request through `SolverService`; only on
verification `unsat`, execute bounded per-rule requests for attribution. Keep
the aggregate raw solver envelope unchanged.

## Task 4: Catalog conformance

**RED:** Tests require every implemented catalog rule to have a compiler,
compile a satisfiable positive model, reject a bound negative model, and cover
its exact boundary.

**GREEN:** Add pure mathematical conformance helpers and fixtures under
`spur-solver`; do not add UI integration fixtures.

## Task 5: Existing MCP surface

**RED:** Schema tests require exactly `solve_rule_spec` and `solve_rules`, keep
`verify|synthesize`, and require family/rule enums to match the registry.

**GREEN:** Derive catalog-facing schema values from the built-in registry and
return structured per-rule verification results without adding a tool or mode.

## Task 6: Generic `layout.axis_capacity`

**SOLVE PRE:** Prove the boundary equation admits exact-fit models and rejects
one-unit overflow for horizontal and vertical axes.

**RED:** Catalog, compiler, boundary, schema, and real-Z3 tests cover
`sum(item extents) + gaps + insets <= available extent` on both axes.

**GREEN:** Add an axis enum and non-negative gap/inset parameters. The first
subject supplies available extent; remaining subjects supply item extents. No
framework object or observation type enters `spur-solver`.

**SOLVE POST:** Re-check the implemented exact-fit and overflow predicates.

**Verification:**

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-solver
scripts/spur-cargo test -p spur-core --test mcp_signals_catalog
scripts/spur-cargo check -p spur-solver -p spur-core
```

Re-run the symbolic task-order constraints after implementation and persist the
final solve artifact. Never treat `unknown` or `timeout` as completion proof.
