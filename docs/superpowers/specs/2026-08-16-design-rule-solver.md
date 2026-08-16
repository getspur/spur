# Design Rule Solver Catalog

**Status:** Approved for implementation
**Beads issue:** `bd-2bx`
**Solver ranking artifact:** `sol_b9b7c440375f4915`
**Post-implementation ordering proof:** `sol_21eddfc43e2147a3`

## Problem

UI layout and graphic-design rules are commonly stated as prose even when the
load-bearing part is geometric: containment, separation, target dimensions,
alignment, aspect ratios, and responsive bounds. Agents need a code-owned rule
catalog and a deterministic compiler so those predicates can be checked or
synthesized with the existing Z3 service.

The generic model-finding service remains domain-neutral. A multi-family rule
catalog lives under `spur_solver::rules` and lowers family-specific predicates
to the existing typed B-prime request model. `design` is the first rule family,
not a one-off solver product.

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
            -> compile predicates to SolveConstraintsRequest
            -> SolverService::solve_constraints
            -> preserve solver status and add domain outcome
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
contains three representative rules rather than an unverified broad catalog:

| Rule | Primitive | Encoding shape |
|---|---|---|
| `layout.containment` | `inside` | Four linear boundary inequalities |
| `layout.non_overlap` | `disjoint` | Four-way Boolean separation |
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
one geometry field and provide a closed integer range.

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

Verification searches for a violation. The compiler asserts the negation of
the conjunction of selected hard rules over fixed facts and bounded unknowns.

| Solver status | Domain outcome |
|---|---|
| `unsat` | `pass` -- no violating assignment exists |
| `sat` | `fail` -- model is a counterexample |
| `unknown` | `unknown` |
| `timeout` | `timeout` |
| `error` | `error` |
| `ended` | `ended` |

### Synthesis

Synthesis asserts all selected hard rules. Preference optimization is deferred
until a separate catalog profile defines its semantics and weighting contract.

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
- Verification wraps the selected hard-rule conjunction in `not`.
- Synthesis sends the hard-rule conjunction directly.
- Rule binding IDs become named constraints where the solver mode supports
  unsat cores.

Raw SMT is not used by the initial catalog.

## Limits and Errors

Requests reuse solver timeout limits. The rule subsystem also bounds scene
nodes, rule bindings, and unknowns with named constants covered by schema and
validation tests. These limits prevent schema and generated-AST blowups; they
are operational safety bounds rather than design recommendations.

Invalid selectors, missing nodes, duplicate unknown paths, invalid ranges,
unsupported binding arity, and invalid dimensions return MCP invalid-params
errors. Solver availability and process failures retain existing solver error
semantics.

## Testing

Every behavior follows red-green-refactor:

1. registry validation and deterministic listing;
2. seed rule detail and examples;
3. `solve_rule_spec` schemas and dispatch;
4. scene validation;
5. each compiler predicate;
6. verification and synthesis status mapping;
7. brain, worker, and catalog registry exposure.

Tests assert feasibility predicates and status semantics, not incidental Z3
model uniqueness.
