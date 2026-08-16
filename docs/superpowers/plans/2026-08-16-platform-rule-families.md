# Platform Rule Families Implementation Plan

**Issue:** `bd-24z`

## Task 1: Family-Neutral Compiler Contract

1. Add failing tests for compiler enumeration, family-discriminated schemas,
   generic model projection, and unchanged design behavior.
2. Introduce `RuleFamilyCompiler`, `FamilyCompilation`, `ModelProjection`, and
   generic compiler lookup.
3. Adapt the design compiler to the shared contract.
4. Compose one global registry from family-owned catalog contributions.
5. Run focused tests and refactor only after green.

## Task 2: Accessibility

1. Add failing catalog tests for four standard-backed rules and metadata.
2. Add failing compile tests for exact boundaries, one-unit violations,
   typed exceptions, evidence requirements, complete verification, and bounded
   synthesis.
3. Implement typed accessibility facts and compiler lowering.
4. Add MCP verification/synthesis tests and run Z3-backed conformance.

## Task 3: Policy

1. Add failing catalog tests for the RBAC profile and four rule definitions.
2. Add failing compile tests for permission reachability, hierarchy cycles,
   static separation, dynamic separation, and bounded membership synthesis.
3. Implement finite graph normalization and typed solver lowering.
4. Add MCP verification/synthesis tests and run Z3-backed conformance.

## Task 4: Resource

1. Add failing catalog tests for capacity and placement profiles.
2. Add failing compile tests for request/limit, aggregate capacity, quota,
   max skew, minimum domains, and bounded numeric synthesis.
3. Implement typed resource facts and solver lowering.
4. Add MCP verification/synthesis tests and run Z3-backed conformance.

## Task 5: Closeout

1. Run the complete `spur-solver` suite with `scripts/spur-cargo`.
2. Run `scripts/spur-cargo fmt --all -- --check` and crate check/clippy as the
   repository allows.
3. Re-run symbolic boundary and infeasibility queries against the shipped
   formulas.
4. Update the canonical solve skill only where navigation guidance is stale.
5. Record test and solve evidence in `bd-24z`, then close only after review.
