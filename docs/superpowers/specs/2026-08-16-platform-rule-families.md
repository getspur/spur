# Platform Rule Families

**Issue:** `bd-24z`

**Status:** Approved for implementation

## Goal

Extend the existing `spur-solver` rule catalog beyond `design` without adding
new MCP tools or runtime integrations. `solve_rule_spec` remains the guide and
`solve_rules` remains the workbench that compiles caller-supplied facts to the
typed solver IR.

The first platform families are:

1. `accessibility`
2. `policy`
3. `resource`

## Non-Goals

- Inspecting a browser, renderer, Kubernetes cluster, IAM service, or policy
  engine.
- Claiming that evidence strings or external facts are true.
- Replacing ordinary schema validation with SMT.
- Adding one MCP tool per family.
- Treating advisory guidance as a hard proof.

## Shared Architecture

Each family implements one internal compiler contract:

```rust
trait RuleFamilyCompiler {
    fn id(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError>;
    fn project_model(
        &self,
        projections: &[ModelProjection],
        model: &BTreeMap<String, ModelValue>,
    ) -> Vec<RuleAssignment>;
}
```

`FamilyCompilation` contains only family-neutral execution state:

- selected mode;
- a validated `SolveConstraintsRequest`;
- identity-preserving `CompiledRule` predicates;
- model projection bindings.

The MCP schema is a top-level `oneOf` whose branches are closed,
family-discriminated request schemas. Adding a family changes neither tool
count nor handler routing.

Catalog metadata distinguishes:

- `availability`: `implemented`, `experimental`, or
  `capability_unavailable`;
- `default_strength`: `hard`, `soft`, or `advisory`;
- `authorities`: standard/version URLs;
- `requires`: facts, exceptions, and evidence required from the caller.

An implemented hard rule may reject a model. Advisory rules are catalog
guidance only until a caller explicitly selects a hard executable rule.
Unavailable rules are discoverable but cannot be passed to `solve_rules`.

## Accessibility Family

### Input

The family accepts a typed scene of viewport facts and elements. Elements may
provide an axis-aligned rectangle and normalized relative luminance values in
the integer range `0..=100_000`. Luminance conversion from colors is an
upstream fact-normalization concern.

Unknown numeric fields are explicitly bounded and limited to geometry and
luminance paths. Verification rejects unknown declarations.

A rule binding may include a typed exception:

```json
{
  "kind": "inline|equivalent|user_agent|essential|two_dimensional",
  "evidence": "caller-owned evidence reference"
}
```

The solver proves the rule conditional on that exception fact. It does not
prove the evidence itself.

### Seed Rules

| Rule | Predicate |
|---|---|
| `a11y.target_size` | `exception OR (width >= min_width AND height >= min_height)`; defaults are the WCAG 2.2 minimum `24 x 24` CSS px |
| `a11y.focus_not_obscured` | focused rectangle is not fully contained by the supplied author-created obscurer |
| `a11y.reflow` | `exception OR content.width <= viewport.width`; callers evaluate the normative 320 CSS px test viewport |
| `a11y.text_contrast` | cross-multiplied WCAG contrast inequality over normalized luminance; default ratio is `4.5:1` encoded as `450` hundredths |

Authorities:

- WCAG 2.2: <https://www.w3.org/TR/WCAG22/>
- Target Size (Minimum):
  <https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html>
- Contrast (Minimum):
  <https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum>

## Policy Family

### Input

The family accepts finite RBAC facts:

- roles with inherited roles and granted permissions;
- principals with assigned roles;
- sessions with active roles and a principal;
- explicitly declared unknown principal-role or session-role memberships.

Graph traversal is finite preprocessing. The compiler lowers the resulting
role closure, membership variables, and rank witnesses to SMT predicates.

### Seed Rules

| Rule | Predicate |
|---|---|
| `rbac.permission_reachable` | at least one assigned role can reach the requested permission through the finite role hierarchy |
| `rbac.role_hierarchy_acyclic` | every inheritance edge respects a strict bounded rank order |
| `rbac.static_separation_of_duty` | selected mutually exclusive assigned roles have cardinality at most `max_assigned` |
| `rbac.dynamic_separation_of_duty` | selected mutually exclusive active session roles have cardinality at most `max_active` |

Authority: NIST RBAC model and separation-of-duty definitions:
<https://csrc.nist.gov/Projects/role-based-access-control/faqs>.

## Resource Family

### Input

The family accepts finite integer resource facts:

- workloads with replicas, per-replica requests, limits, and domain counts;
- pools and quotas with named resource capacities;
- explicitly bounded workload numeric unknowns.

All resource names and workload memberships are explicit. The compiler does
not query a scheduler or infer ownership.

### Seed Rules

| Rule | Predicate |
|---|---|
| `resource.request_within_limit` | every named request is at most its matching limit |
| `resource.aggregate_capacity` | sum of `replicas * request` for bound workloads is at most pool capacity for every named resource |
| `resource.quota_capacity` | the same aggregate demand is at most the selected quota |
| `placement.topology_max_skew` | every pair of declared domain counts differs by at most `max_skew` |
| `placement.minimum_failure_domains` | the number of domains with positive placement count is at least `minimum_domains` |

Authorities:

- Kubernetes resource requests and limits:
  <https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/>
- Kubernetes topology spread constraints:
  <https://kubernetes.io/docs/concepts/scheduling-eviction/topology-spread-constraints/>

## Verification Semantics

For `verify`, caller-owned facts are fixed by equality constraints. Internal
witness variables, such as role ranks and domain-presence flags, remain solver
variables. `sat` means every selected hard predicate holds; `unsat` means the
complete supplied facts violate at least one selected rule. Each hard binding
is re-solved independently on aggregate `unsat` for attribution.

## Synthesis Semantics

For `synthesize`, only explicitly declared unknown facts remain free and every
unknown has a closed domain. `sat` returns projected assignments; `unsat`
means no assignment in those declared bounds satisfies all selected hard
rules. First `sat` is feasibility, not a unique optimum.

## Limits

Family limits derive from the existing solver caps (`MAX_VARIABLES` and
`MAX_CONSTRAINTS`). Compilers fail before execution when a request would
exceed the shared typed-IR limits.

## Testing

- Catalog tests cover every family, profile, rule ID, authority, availability,
  and strength.
- Each production behavior begins with a failing test.
- A family-neutral conformance harness runs one exact boundary and one
  counterexample for every implemented hard rule.
- MCP tests cover each schema branch, verification attribution, synthesis
  projection, unknown-family rejection, and unsupported-rule rejection.
- Post-implementation solve checks re-evaluate standard boundaries and one
  infeasible case per family.
