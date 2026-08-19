# Solve Constraint Authoring Contract — Implementation Plan

**Date:** 2026-08-19
**Design:** [2026-08-19-solve-constraint-authoring-contract-design.md](../specs/2026-08-19-solve-constraint-authoring-contract-design.md)
**Implementation issue:** `bd-gfye5`

## Objective

Make the generic solver request language discoverable and repairable for agents
without weakening provider compatibility or changing solver semantics. Add a
progressive language catalog, a validation-only preflight, canonical published
constraint entries, and structured invalid-parameter diagnostics.

## Constraints

- Keep changes scoped to `spur-solver`, solver MCP registration tests, and the
  bundled `solve` skill unless an integration test proves a wider seam.
- Preserve runtime support for legacy bare `ConstraintExpr` entries.
- Do not introduce `oneOf`, `allOf`, or JSON Schema type unions; supported
  Bedrock/Kiro workers reject them.
- `solve_constraint_check` must not acquire or call `SolverService`.
- Use `scripts/spur-cargo`; never invoke bare `cargo`.
- Notebook MCP is unavailable (`Transport closed`), so the approved Markdown
  design is authoritative for this implementation.

## Task 1 — Pin the public contract in failing tests

**Files:**

- Modify `crates/spur-solver/src/mcp.rs`
- Add `crates/spur-solver/src/constraint_spec.rs`
- Modify `crates/spur-solver/src/lib.rs`

**RED assertions:**

1. Tool order is exactly `solve_rule_spec`, `solve_rules`,
   `solve_constraint_spec`, `solve_constraint_check`, `solve_constraints`,
   `solve_smt`, `get_solve_result`.
2. The execution and preflight schemas publish only wrapped top-level
   constraints and require `expr`.
3. The shared expression `value` property does not falsely claim one JSON type.
4. Empty `solve_constraint_spec` returns a bounded versioned summary; one
   variable, expression, operator, request, limits, and examples selector each
   returns deterministic detail.
5. Multiple or unknown selectors return stable selector diagnostics.
6. Valid canonical examples deserialize and pass semantic validation.
7. One-defect fixtures produce structured paths for missing variant fields,
   wrong literal types, wrapper/bare mixing, and operator arity.
8. `solve_constraint_check` succeeds on a catalog-only module, proving it has no
   live-service dependency.

Run each focused test and confirm it fails for the expected missing behavior
before implementation.

## Task 2 — Implement the code-owned language catalog

**Files:**

- Add `crates/spur-solver/src/constraint_spec.rs`
- Modify `crates/spur-solver/src/lib.rs`

Create a compact registry for:

- variable variants and their required/irrelevant fields;
- expression variants and kind-specific JSON value types;
- operators, exact arity, accepted operand sorts, result sort, and examples;
- request sections, enforced limits, and valid/invalid examples.

Expose a serde request with mutually exclusive selectors and an include level.
Return versioned JSON envelopes with normalized query metadata, capability, and
next-tool hints. Keep exhaustive lists in code, not in the skill.

Add coverage tests that pin every currently supported variable kind,
expression kind, and operator, and compare limit cards to the constants enforced
by `types.rs`.

## Task 3 — Add preflight and structured diagnostics

**Files:**

- Modify `crates/spur-solver/src/constraint_spec.rs`
- Modify `crates/spur-solver/src/mcp.rs`

Add a recursive raw-JSON shape checker driven by the catalog metadata before
serde deserialization. Return `-32602` with diagnostic `data` containing stable
`code`, `phase`, `path`, and `message`, plus repair fields when useful.

After deserialization, call `SolveConstraintsRequest::validate` and map its
stable validation kind and path into the same envelope. Reuse this parsing path
for both `solve_constraint_check` and `solve_constraints`, while leaving actual
execution behavior unchanged.

The check response reports counts, soft/optimization flags, schema version, and
`next_tools: ["solve_constraints"]`; it never resolves a Z3 binary, touches the
cache, persists a result, or calls a service.

## Task 4 — Publish the canonical provider-compatible schema

**Files:**

- Modify `crates/spur-solver/src/mcp.rs`

Add definitions and dispatch for the two new tools. Share the execution request
schema with the check tool. Require `expr` in every published constraint entry,
retain optional metadata, and omit a single declared type for the polymorphic
expression `value` field. Point descriptions to `solve_constraint_spec` for
kind-specific contracts.

Keep the schema flat and provider-compatible. Update module docs and all exact
schema/tool-order tests.

## Task 5 — Update integration expectations and the solve skill

**Files:**

- Modify `crates/spur-core/src/mcp/mod.rs` if registration tests require it
- Modify `crates/spur-core/tests/mcp_signals_catalog.rs` if exact catalogs require it
- Modify `crates/spur-cli/assets/skills/solve/SKILL.md`

Update registry tests so the spec and check tools are discoverable and callable
without a live solver. Revise the skill's generic fallback to:

```text
catalog miss
  -> solve_constraint_spec summary
  -> narrow unfamiliar entry
  -> author named wrapped hard constraints
  -> solve_constraint_check
  -> solve_constraints hard feasibility
  -> add preferences/objectives
  -> re-check and re-solve
```

Remove the claim that the flat execution schema is the complete grammar. Keep
catalog-first routing, proof/status discipline, optimization interpretation,
persistence, and raw-SMT guidance intact.

Before editing the skill, capture a baseline agent-authoring failure without the
new guidance. After editing it, rerun the same pressure scenario and verify the
agent chooses progressive spec lookup and preflight rather than guessing the
payload.

## Task 6 — Verification and record

1. Run `scripts/spur-cargo fmt --all -- --check` (format first if needed).
2. Run focused `spur-solver` contract tests.
3. Run `scripts/spur-cargo test -p spur-solver`.
4. Run affected `spur-core` MCP tests.
5. Run the provider schema compatibility test that forbids unsupported unions.
6. Run `git diff --check` and inspect only task-owned paths.
7. Record verification evidence on `bd-gfye5`; close it only when all acceptance
   criteria pass.

## Completion criteria

- Agents can discover exact generic-language variants and operators without
  reading Rust source.
- Canonical examples preflight successfully and execute unchanged.
- Common malformed payloads return repairable, path-aware diagnostics rather
  than an untagged-enum dead end.
- The validation-only tool operates in catalog-only mode.
- Worker-provider schemas remain compatible.
- The bundled solve skill requires spec lookup and preflight before generic
  execution.
