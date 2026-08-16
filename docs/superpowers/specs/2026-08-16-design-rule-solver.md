# Design Rule Solver Catalog

**Status:** Implemented
**Beads issue:** `bd-2bx`
**Solver ranking artifact:** `sol_b9b7c440375f4915`
**Post-implementation ordering proof:** `sol_21eddfc43e2147a3`
**Solver-only task ranking:** `sol_3430e4c7c5d74c73`
**Axis-capacity proof artifacts:** `sol_2367ba362d8a462e`,
`sol_168998e1847e448e`, `sol_37ec96b05dea413c`, `sol_49ed76c5de1e4e95`

## Problem

Layout and graphic-design rules are commonly stated as prose even when their
load-bearing part is mathematical: containment, separation, capacity, aspect
ratios, and responsive bounds. Agents need a code-owned rule catalog and a
deterministic compiler so declared facts can be evaluated or completed with the
existing Z3 service.

The generic model-finding service remains domain-neutral. A multi-family rule
catalog lives under `spur_solver::rules` and lowers family-specific predicates
to the existing typed B-prime request model. `design` is the first rule family,
not a one-off solver product.

The rule subsystem is a mathematical workbench, not an observation system. It
does not inspect DOM, Ratatui, Figma, screenshots, or framework objects. Callers
own conversion of external state into declared facts. A solver result therefore
describes only the supplied model under the selected rules; it does not claim
subjective UI quality or completeness of an external observation process.

## Public MCP Surface

Exactly two tools are added:

| Tool | Responsibility |
|---|---|
| `solve_rule_spec` | Read-only catalog discovery and progressive rule guidance |
| `solve_rules` | Verify facts or synthesize unknowns for one rule family |

There are no separate list, get, compile, explain, or relaxation tools.
Existing `solve_constraints`, `solve_smt`, and `get_solve_result` remain the
advanced solver surface.

## Architecture

```text
solve_rule_spec -> static versioned registry

solve_rules -> select family compiler
            -> validate family facts and selected rules
            -> preserve rule identity while compiling predicates
            -> compile one aggregate SolveConstraintsRequest
            -> SolverService::solve_constraints
            -> preserve solver status and add mode-specific outcome
            -> attribute invalid complete models to individual rules
```

`crates/spur-solver/src/rules` owns generic registry types, family routing,
domain diagnostics, and the MCP adapters. Family implementations live below
`rules/families`; `rules/families/design` owns scene types and geometry
compilation. Rule code delegates to `SolverService` and adds no process state.

## `solve_rule_spec`

The request follows the progressive-disclosure pattern established by
`notebook_ggsql_spec`:

```json
{
  "family": "design",
  "profile": "geometric_integrity",
  "rule_id": "layout.containment",
  "primitive": "inside",
  "include": "summary|valid_example|invalid_example|llm_encoding|solver_encoding|all"
}
```

At most one of `family`, `profile`, `rule_id`, or `primitive` may be present. An
empty request returns bounded family cards. A family selector lists its bounded
profiles. Exact selectors return stable errors for unknown IDs. Responses include:

- `registry_schema_version` and per-rule version;
- capability state: `implemented`, `experimental`, or
  `capability_unavailable`;
- standards authority and applicability conditions;
- required facts and parameters;
- valid and invalid examples;
- LLM problem shapes, encoding steps, anti-patterns, and escalation guidance;
- solver theory, formula, and verification/synthesis strategies;
- `next_tools`.

## Initial Family and Catalog

The first family is `design`; its first profile is `geometric_integrity`. It
contains four representative rules rather than an unverified broad catalog:

| Rule | Primitive | Encoding shape |
|---|---|---|
| `layout.containment` | `inside` | Four linear boundary inequalities |
| `layout.non_overlap` | `disjoint` | Four-way Boolean separation |
| `layout.axis_capacity` | `axis_capacity` | Item extents, gaps, and insets fit one available extent |
| `media.aspect_ratio` | `aspect_ratio` | Integer cross multiplication |

Accessibility and subjective visual-rhythm profiles are deferred until their
applicability and normative-source mappings are reviewed independently.

## Scene Model

The scene is normalized and typed:

```json
{
  "viewport": {"width": 390, "height": 844},
  "nodes": {
    "panel": {"rect": {"x": 0, "y": 0, "width": 390, "height": 844}},
    "button": {"parent": "panel", "rect": {"x": 320, "y": 780, "width": 44, "height": 44}}
  }
}
```

Coordinates and dimensions are integers. Width and height must be positive;
coordinates may be negative for off-canvas counterexamples. Node IDs are
unique map keys and parent references must resolve. Synthesis unknowns identify
one geometry field and provide a closed integer range. Verification rejects
unknown geometry and requires a complete model. Synthesis rejects implicit or
unbounded unknowns.

Rules are applied explicitly through bindings. Contextual rules such as
non-overlap are never inferred for every pair of nodes.

## `solve_rules`

```json
{
  "family": "design",
  "mode": "verify",
  "rules": [
    {"rule_id": "layout.containment", "subjects": ["button", "panel"]}
  ],
  "scene": {},
  "unknowns": [],
  "timeout_ms": 30000,
  "persist": false,
  "include_smt": false
}
```

### Verification

Verification evaluates one complete supplied model. The compiler asserts every
selected hard rule directly. Unknown declarations are invalid in this mode.

| Solver status | Domain outcome |
|---|---|
| `sat` | `pass` -- the complete model satisfies every selected rule |
| `unsat` | `fail` -- at least one selected rule rejects the complete model |
| `unknown` | `unknown` |
| `timeout` | `timeout` |
| `error` | `error` |
| `ended` | `ended` |

### Synthesis

Synthesis asserts all selected hard rules over complete facts and explicitly
declared bounded unknowns. A satisfiable result proves that at least one valid
completion exists; it does not prove that an arbitrary external UI passes.
Preference optimization is deferred until a separate catalog profile defines
its semantics and weighting contract.

| Solver status | Domain outcome |
|---|---|
| `sat` | `solution` |
| `unsat` | `infeasible` |
| `unknown` | `unknown` |
| `timeout` | `timeout` |
| `error` | `error` |
| `ended` | `ended` |

The raw `status` is always returned unchanged. `unknown`, `timeout`, and
`ended` are never interpreted as proof.

## Constraint Encoding

- Containment names each rule binding and lowers rectangle edges to integer
  additions and comparisons.
- Non-overlap uses a four-way `or`: left, right, above, or below, including a
  non-negative minimum gap.
- Aspect ratio avoids division: `render_width * source_height =
  render_height * source_width`.
- Axis capacity uses `sum(item extents) + gap * (item count - 1) + start inset
  + end inset <= available extent` for either declared axis.
- Verification and synthesis both assert selected predicates directly.
- Every compiled predicate retains its stable rule ID and binding index.
- Aggregate constraints use stable per-binding IDs. Invalid verification can
  run bounded per-rule queries for exact attribution.

Raw SMT is not used by the initial catalog.

## Limits and Errors

Requests reuse solver timeout limits. The rule subsystem also bounds scene
nodes, rule bindings, and unknowns with named constants covered by schema and
validation tests. These limits prevent schema and generated-AST blowups; they
are operational safety bounds rather than design recommendations.

Invalid selectors, missing nodes, duplicate unknown paths, invalid ranges,
unknowns in verification, implicit unknowns in synthesis, unsupported binding
arity, and invalid dimensions return MCP invalid-params errors. Solver
availability and process failures retain existing solver error semantics.

## Testing

Every behavior follows red-green-refactor:

1. registry validation and deterministic listing;
2. seed rule detail and examples;
3. `solve_rule_spec` schemas and dispatch;
4. scene validation;
5. identity-preserving compiler predicates and catalog/compiler conformance;
6. complete-model verification and bounded synthesis status mapping;
7. per-rule attribution for invalid complete models;
8. registry-derived MCP schemas and brain/worker exposure.

Tests assert feasibility predicates and status semantics, not incidental Z3
model uniqueness. Test models are pure solver inputs; no renderer or UI
integration fixtures are part of this crate.
