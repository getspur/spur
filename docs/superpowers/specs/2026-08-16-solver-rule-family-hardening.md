# Solver Rule Family Hardening

## Goal

Make the accessibility, policy, and resource rule families reliable under malformed facts, synthesis, standards-derived boundaries, and multi-rule failure attribution. The solver remains a generic workbench over caller-supplied normalized facts.

## Correctness Contract

1. Capacity rules reject an explicitly selected resource that is absent from the selected pool or quota. Compilation must never index an unchecked map key.
2. A synthesis unknown may only target a declared null field. Unknowns over concrete accessibility values are rejected instead of being projected as unconstrained assignments.
3. Every fixed RBAC session role must be authorized for that session's principal. Authorization includes directly assigned roles and roles inherited by an assigned role.
4. Every evaluated placement model conserves replicas: the sum of declared domain counts equals `workload.replicas`.
5. Reflow constrains the full horizontal extent, `x >= 0` and `x + width <= viewport.width`. Target size accepts the WCAG spacing exception when the caller supplies typed evidence.
6. `placement.topology_max_skew` requires `max_skew > 0`. Its catalog text describes declared domains and replica conservation precisely rather than claiming scheduler integration.
7. Policy and resource catalog examples contain complete, schema-valid `solve_rules` request fragments and encode a genuine passing and failing boundary.
8. Verification uses one end-to-end timeout budget for the aggregate solve and all per-rule attribution solves. Once no budget remains, unresolved rule results are timeouts without further backend calls.

## Scope

- Change only `spur-solver` and its solver documentation.
- Preserve stable family and rule IDs.
- Preserve the single `solve_rules` and `solve_rule_spec` tool surface.
- Add no Ratatui, browser, framework, or scheduler integration fixtures.
- Keep standards exceptions explicit and caller-owned; the solver validates the encoded evidence but does not inspect a live UI.

## Verification

Each defect receives a regression test that fails against the current implementation. After the implementation passes those tests, replay the reviewed adversarial models through `solve_rules`, run all `spur-solver` tests, format, and run clippy for the crate.
