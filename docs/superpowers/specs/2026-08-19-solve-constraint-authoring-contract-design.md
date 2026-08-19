# Solve Constraint Authoring Contract — Design

**Status:** Implemented
**Date:** 2026-08-19
**Design epic:** `bd-a9v3y`
**Implementation issue:** `bd-gfye5`
**Supersedes in part:** [2026-08-16 Solve Skill Catalog Navigator](./2026-08-16-solve-skill-catalog-navigator.md)

> Notebook MCP was retried twice and returned `Transport closed`. This legacy
> Markdown artifact is the authoritative fallback. No `.ipynb` or unverified
> Markdown Mermaid was hand-authored.

## Problem

The generic `solve_constraints` path is difficult for agents to call correctly.
Its worker-compatible JSON schema is intentionally flat because Kiro/Bedrock
rejects `oneOf`, `allOf`, and type unions, while the Rust request uses exact
tagged unions. The published schema therefore accepts shapes the runtime rejects
and cannot express the complete recursive grammar.

Observed live on 2026-08-19:

| Request shape | Published schema | Runtime result |
|---|---|---|
| `{kind:"bool", value:true}` | incorrectly describes `value` as integer | `sat` |
| `{kind:"bool", value:1}` | accepted by the flat field schema | generic untagged-enum error |
| `int_range` without `min`/`max` | only `type` and `name` required | `missing field min` |
| wrapper metadata plus bare `kind`/`op` | allowed by merged properties | generic untagged-enum error |
| `ge` with one argument | `args.minItems = 1` | useful semantic arity error |

The semantic validator is effective after deserialization. The high-friction
failure surface is authoring discovery plus pre-semantic parsing.

## Goals

1. Give agents progressive, versioned access to the generic B-prime language.
2. Publish one canonical constraint-entry form without removing legacy runtime
   compatibility.
3. Provide a validation-only preflight that never launches Z3.
4. Return structured, path-aware, repairable invalid-parameter diagnostics.
5. Keep the foundation `solve` skill small and catalog-first.
6. Prove schema, examples, deserialization, semantic validation, and live tool
   behavior stay aligned.

## Non-goals

- Changing solver semantics, supported theories, or Optimize behavior.
- Removing legacy bare `ConstraintExpr` entries in this version.
- Adding JSON Schema unions rejected by supported worker providers.
- Duplicating domain-rule formulas from `solve_rule_spec`.
- Replacing tests with solver proofs or schema validation.

## Decision

Add two read-only/preflight tools next to the existing execution tools:

| Tool | Responsibility |
|---|---|
| `solve_constraint_spec` | Progressive discovery of generic request, variable, expression, operator, limits, and example contracts |
| `solve_constraint_check` | Deserialize and semantically validate a request without acquiring the Z3 semaphore or launching a process |

`solve_constraints` remains the execution endpoint. `solve_rule_spec` remains
the domain-rule catalog. The two spec tools have distinct ownership: rule
formulas versus generic language grammar.

## Routing contract

The agent procedure is normative:

1. Call `solve_rule_spec({})` for every new rule-shaped task.
2. If an implemented catalog rule matches, call `solve_rules`.
3. Otherwise, if the typed language can express the problem:
   1. call `solve_constraint_spec({})`;
   2. narrow only the unfamiliar variable, expression, operator, request, or
      example entry;
   3. author the canonical request;
   4. call `solve_constraint_check`;
   5. call `solve_constraints` only after a valid preflight.
4. Use `solve_smt` only for an unsupported theory.

For optimization, agents first establish hard feasibility, then add soft
constraints or objectives and preflight again.

## `solve_constraint_spec` request

The request uses the same progressive-disclosure pattern as `solve_rule_spec`.
With no selector it returns a bounded catalog summary. Exactly one selector may
be supplied.

```json
{
  "section": "request|limits|examples",
  "variable": "bool|int|int_range|enum|real|bit_vec",
  "expression": "var|int|bool|enum_label|real|bv|op",
  "operator": "eq|ne|lt|le|gt|ge|add|sub|mul|and|or|not|bv_and|...",
  "include": "summary|valid_example|invalid_example|all"
}
```

Rules:

- Selectors are mutually exclusive.
- `include` defaults to `summary`.
- Unknown selectors are invalid parameters with structured diagnostic data.
- Every response includes `language_schema_version`, normalized query metadata,
  capability status, and `next_tools`.
- Operator detail includes exact arity, accepted operand sorts, result sort, and
  at least one valid expression.
- Variant detail includes required fields, forbidden/irrelevant fields, and a
  valid JSON value.
- `limits` is generated from the constants enforced by the service.

The registry is code-owned. The skill contains no exhaustive variant or operator
list.

## Canonical request subset

The published `solve_constraints` and `solve_constraint_check` schemas advertise
only wrapped top-level constraints:

```json
{
  "vars": [
    {"type":"int_range", "name":"x", "min":0, "max":10}
  ],
  "constraints": [
    {
      "id":"x_at_least_three",
      "expr": {
        "kind":"op",
        "op":"ge",
        "args":[
          {"kind":"var", "name":"x"},
          {"kind":"int", "value":3}
        ]
      }
    }
  ]
}
```

`expr` is required in the advertised constraint item. `id`, `group`, `soft`,
and `weight` retain their existing semantics. The runtime continues accepting a
bare `ConstraintExpr` for backward compatibility, but the schema and examples do
not encourage two competing authoring forms.

Because provider-compatible schemas cannot express the Boolean/integer union for
the shared `value` field, that property's schema does not lie about a single JSON
type. Its description points to `solve_constraint_spec` for kind-specific type
information. Recursive `args` likewise remain object-shaped at the provider
boundary and are specified progressively by the language tool.

## Validation-only response

Valid requests return a small deterministic envelope:

```json
{
  "valid": true,
  "language_schema_version": 1,
  "summary": {
    "variables": 1,
    "constraints": 1,
    "objectives": 0,
    "has_soft_constraints": false,
    "is_optimization": false
  },
  "next_tools": ["solve_constraints"]
}
```

The check performs deserialization and the same `SolveConstraintsRequest::validate`
used by the encoder. It does not discover Z3, check its version, acquire the
semaphore, touch the request cache, create a session, persist a result, or encode
and launch a script.

## Diagnostic contract

Invalid-parameter MCP errors keep JSON-RPC code `-32602` and populate `data`:

```json
{
  "code": "missing_variant_field",
  "phase": "deserialize",
  "path": "constraints[0].expr.args[0].name",
  "message": "kind=var requires field name",
  "expected": "name: identifier",
  "found": "var",
  "hint": "Use name for a variable reference; var is used by enum_label.",
  "example": {"kind":"var", "name":"x"}
}
```

Required fields:

| Field | Meaning |
|---|---|
| `code` | Stable machine-readable category |
| `phase` | `deserialize`, `semantic`, or `selector` |
| `path` | JSON-like location; root is `$` |
| `message` | Concise explanation |

Optional repair fields are `expected`, `found`, `hint`, and `example`.
Semantic validation errors preserve the existing stable `ValidationErrorKind`
name and path instead of flattening them into one string.

Shape diagnostics are driven by the same code-owned language metadata returned
by `solve_constraint_spec`. Common tagged-union mistakes must not fall through to
`data did not match any variant of untagged enum ConstraintItem`.

## Compatibility and migration

- Existing valid `solve_constraints` requests retain runtime behavior.
- Legacy bare constraints remain deserializable and executable.
- Tool ordering becomes:
  `solve_rule_spec`, `solve_rules`, `solve_constraint_spec`,
  `solve_constraint_check`, `solve_constraints`, `solve_smt`,
  `get_solve_result`.
- Existing clients that discover tools by name are unaffected.
- Snapshot/schema tests are updated deliberately for the new tools and canonical
  advertised subset.
- The 2026-08-16 decision that “no MCP tool is added” is superseded only for the
  generic-language authoring gap; catalog ownership remains unchanged.

## Skill contract

The bundled `solve` skill must no longer say the flat execution schema is the
complete grammar. Its generic fallback becomes:

```text
catalog miss
  -> solve_constraint_spec summary
  -> narrow unfamiliar language entry
  -> author bounded vars + named wrapped hard constraints
  -> solve_constraint_check
  -> solve_constraints hard feasibility
  -> add preferences/objectives only after feasibility
  -> re-check and re-solve
```

The skill retains status semantics, proof discipline, persistence guidance, and
the raw SMT escape hatch. It does not duplicate the full operator catalog.

## Test strategy

### Contract tests

1. Every published valid example satisfies the advertised schema subset.
2. Every published valid example deserializes and passes semantic validation.
3. Canonical execution examples reach the fake/live service path as appropriate.
4. Registry coverage tests pin all variable kinds, expression kinds, and
   operators, including arity and result sort.
5. Limit values equal the constants enforced by `types.rs`.

### Negative fixtures

At minimum, one-defect fixtures cover:

- missing `int_range.min` or `.max`;
- `var` expression using `var` instead of `name`;
- Boolean literal with numeric `value`;
- wrapper metadata without `expr`;
- unknown operator;
- wrong operator arity;
- mixed numeric sorts;
- undeclared variable;
- invalid enum label;
- soft weight/group misuse.

Each fixture asserts JSON-RPC code, diagnostic phase, stable code, exact path,
and a non-empty repair hint when the defect is mechanically repairable.

### Agent evaluations

Run the same constraint-authoring prompts without and with the new spec/check
workflow. Acceptance requires:

- all canonical examples succeed on the first execution call after preflight;
- no evaluation uses raw SMT when B-prime is sufficient;
- malformed fixtures are repaired from returned data without reading crate
  source;
- `unknown` and `timeout` are never interpreted as `unsat`.

## Acceptance criteria

1. The two new tools are live in brain and worker registries.
2. Provider schema compatibility tests remain green.
3. The Boolean schema/runtime contradiction is removed.
4. Canonical constraints require `expr` in the advertised schema.
5. Invalid authoring shapes return structured, path-aware diagnostics.
6. Validation-only requests launch no solver process.
7. The bundled skill uses progressive generic-language discovery and preflight.
8. `scripts/spur-cargo test -p spur-solver` passes.
9. Relevant `spur-core` registry/schema tests pass.
10. Workspace formatting and targeted clippy pass.

## Risks

| Risk | Mitigation |
|---|---|
| Grammar metadata drifts from serde variants | Coverage tests and shared variant/operator metadata |
| More tools increase prompt cost | Bounded descriptions; details loaded only on demand |
| Legacy callers depend on bare constraints | Preserve runtime form; change only advertised canonical subset |
| Provider rejects richer schemas | Continue flat provider-compatible schema; move conditional grammar to spec tool |
| Diagnostics expose unstable serde wording | Stable code/path envelope owned before serde text |
