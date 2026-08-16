# Solver Rule Family Hardening Plan

**Spec:** `docs/superpowers/specs/2026-08-16-solver-rule-family-hardening.md`

## Task 1: Input Safety

Add failing compiler tests for an absent selected capacity resource and an accessibility unknown targeting a concrete value. Implement deterministic validation and retain valid synthesis coverage.

## Task 2: Policy Authorization

Add failing tests for an unauthorized active session role plus passing direct and inherited authorization cases. Validate fixed session activation against the principal's transitive role authorization.

## Task 3: Placement Semantics

Add failing tests for non-conserving domain counts and `max_skew = 0`. Conjoin replica conservation with placement predicates, require positive skew, and update the schema/catalog formula.

## Task 4: Accessibility Semantics

Add failing reflow extent and target spacing-exception tests. Compile full horizontal extent constraints and add a typed `spacing` exception accepted only by target-size rules.

## Task 5: Executable Catalog Examples

Add catalog tests that deserialize and compile every policy/resource example. Replace placeholders with rule-specific request fragments whose documented outcomes are solver-backed.

## Task 6: Attribution Deadline

Add unit tests around remaining-budget calculation and request timeout propagation, followed by an execution regression that exhausts attribution budget. Use a monotonic deadline shared by aggregate and serial attribution solves.

## Task 7: Full Verification

Replay adversarial `solve_rules` requests, run `scripts/spur-cargo test -p spur-solver`, run formatting, and run crate clippy with warnings denied. Record results on the tracked remediation issues.
