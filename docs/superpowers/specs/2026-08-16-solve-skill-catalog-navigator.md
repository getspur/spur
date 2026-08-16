# Solve Skill Catalog Navigator

**Status:** Implemented
**Date:** 2026-08-16
**Issue:** `bd-3l3`

## Problem

The original `solve` foundation skill was a self-contained solver reference. It
duplicated generic grammar, examples, and domain patterns while omitting the
new `solve_rule_spec` and `solve_rules` surfaces. Because foundation skills are
projected broadly, this imposed prompt cost and allowed skill guidance to drift
from the versioned rule registry.

## Decision

Keep `solve` as an always-available, catalog-first router:

1. Recognize constraint-shaped work.
2. Discover catalog capabilities with `solve_rule_spec`.
3. Execute an implemented catalog match with `solve_rules`.
4. Fall back to `solve_constraints` for uncatalogued constraints.
5. Use `solve_smt` only for theories outside the typed surface.

No MCP tool is added. `solve_rule_spec` is the navigation endpoint for the
mathematical rule catalog; `skill_navigate` remains the navigation endpoint for
procedural skills.

## Ownership Boundaries

| Owner | Content |
|---|---|
| `solve` skill | Triggering, routing, status interpretation, proof discipline, handoff |
| Rule registry | Families, profiles, rule definitions, authority, examples, encodings |
| MCP tool schemas | Current typed request grammar and bounds |
| Solver service | Execution, raw status, model, timeout, persistence |

The skill must not contain an exhaustive rule list or duplicate catalog
formulas. Generated `.codex`, `.claude`, `.kimi`, and other adapter projections
are outputs; only `assets/skills/solve/SKILL.md` is canonical.

## Progressive Navigation

Start with `solve_rule_spec({})`, narrow with one of `family`, `profile`,
`rule_id`, or `primitive`, and request `summary` before loading one detailed
view. Detailed views are `valid_example`, `invalid_example`, `llm_encoding`,
and `solver_encoding`; `all` is reserved for full audits.

## Status Semantics

Catalog rule execution and generic proof queries are deliberately distinct:

| Query | Satisfiable | Unsatisfiable |
|---|---|---|
| `solve_rules` verify | `pass` | `fail` |
| `solve_rules` synthesize | `solution` | `infeasible` |
| Generic feasibility | valid model | no valid model |
| Generic counterexample query | counterexample | property proven in encoded domain |

`unknown`, `timeout`, `error`, and `ended` are never proof outcomes.

## Acceptance

- Catalog discovery and execution precede generic hand encoding.
- The foundation skill contains no static domain-rule catalog.
- The TDD skill uses the same catalog-first preflight.
- Canonical bundled-skill tests pin routing and status semantics.
- Skill validation, formatter, and relevant Rust tests pass.
