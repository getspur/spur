# Domain-level Z3 Optimize Objective Rules Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-24-domain-z3opt-objective-rules-design.ipynb`
**Formal @spec cells:** `OBJECTIVE-ELIGIBILITY`, `OPTIMIZATION-LIFECYCLE`, `OBJECTIVE-RELEASE-GATE`
**Design epic:** `bd-27mx` (closed)

**Goal:** Add a reusable, manifest-declared domain objective contract, make `rbac.minimum_privilege` executable, and add `placement.minimize_skew` without weakening existing hard constraints.

**Architecture:** Extend strict rule manifests with a defaulted `execution_kind`, carry that classification through runtime binding and catalog projection, and centralize single-minimize lowering in `rules::primitives`. Policy and resource compilers add one synthesis-only objective while continuing to emit all feasibility, authorization, capacity, and placement predicates as named hard constraints.

**Tech Stack:** Rust 2021, Serde/YAML manifests, typed `spur-solver` constraint IR, Z3 Optimize, Tokio integration tests, SPUR MCP rule execution.

**Measured baseline:** The current worktree has 8 families, 40 rule manifests, 39 executable handlers, and one catalog-only rule (`rbac.minimum_privilege`). Wave 1 ends with 41 rule manifests and 41 executable handlers.

---

## File map

- `crates/spur-solver/src/rules/manifest_format.rs`: strict manifest schema, executable routing, handler enum, and handler ABI.
- `crates/spur-solver/src/rules/manifest.rs`: embedded-manifest lookup, validated binding metadata, and catalog conversion.
- `crates/spur-solver/src/rules/catalog.rs`: public serialized guidance, including objective discoverability.
- `crates/spur-solver/src/rules/primitives.rs`: family-neutral construction of one typed minimize objective.
- `crates/spur-solver/src/rules/families/policy/compile.rs`: minimum-privilege utility validation and cost objective.
- `crates/spur-solver/src/rules/families/resource/compile.rs`: skew variable, conservation predicates, objective, and projection.
- `crates/spur-solver/src/rules/families/*/rules/*.yaml`: authoritative objective rule contracts and conformance requests.
- `crates/spur-solver/tests/*manifest*`: strict routing, dispatch, ownership, counts, and frozen catalog coverage.
- `crates/spur-solver/tests/domain_objective_release.rs`: cross-domain optimality, rejection, ratchet, and termination release gate.

## Dependency DAG

```text
task-0a ──┐
          ├──> task-1 ──> task-2 ──┬──> task-4 ──> task-5 ──┬──> task-8
task-0b ──┘                         │                        └──> task-9
    └────────> task-3 ──────────────┤
                                    └──> task-6 ──> task-7 ──┬──> task-8
                                                             ├──> task-9
task-4 ──────────────────────────────────────┐
task-6 ──────────────────────────────────────┴──> task-10
```

Tasks 0A and 0B can run in parallel. Task 1 waits for both handler-match refactors so adding closed enum variants remains commit-ready; Task 3 follows 0B because both touch scheduling. Tasks 4 and 6 can run in parallel after Tasks 2 and 3. Policy and resource catalog tasks remain isolated. Tasks 8, 9, and 10 can run in parallel after their stated prerequisites.

## Mandatory per-task execution protocol

The user selected `codex` with model `gpt-5.6-sol` and `xhigh` effort for every task. Each worker must follow this evidence order and record it in the beads issue before requesting review:

1. **Pre-solve:** call `solve_rule_spec` first, then run and persist the closest bounded `solve_rules` or `solve_constraints` probe for the task's invariant. For catalog/refactor work, use a representative rule from the family being protected. Record the rule/profile, solver status, `solve_id`, model/objective value when present, and why the probe is relevant. `unknown` or timeout is never proof.
2. **RED:** add or tighten the smallest focused automated test and run it to observe the expected failure. Characterization-only refactors must first preserve a passing baseline, then introduce a focused structural/ownership assertion that fails before the edit.
3. **GREEN:** implement the minimum scoped production change and rerun the focused test through `scripts/spur-cargo`.
4. **Post-solve:** rerun the same persisted solve probe against the implemented contract. Its expected feasibility/optimum/termination must match the task acceptance criteria; explain any model delta. This does not replace tests.
5. **Refactor and verify:** keep behavior-preserving cleanup separate, run every listed task command plus formatting, inspect the diff for scope, commit only the task files, and attach command/solve evidence to the issue.

No production edit may precede the pre-solve and RED evidence. If a task cannot formulate a bounded relevant probe, emit a `risk` signal instead of silently skipping solve.

---

### Task 0A: Isolate foreign handlers in accessibility, configuration, design, and policy

**Task ID:** `task-0a`

**Files:**
- Modify: `crates/spur-solver/src/rules/families/accessibility/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/configuration/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/design/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/policy/compile.rs:387-511`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Each compiler continues to match every handler owned by its family explicitly.
- [ ] The enumerated list of every foreign-family handler is replaced by one stable unsupported-rule fallback.
- [ ] Manifest family/handler validation remains the authority preventing cross-family dispatch.
- [ ] Existing accessibility, configuration, design, and policy tests pass without response or diagnostic changes.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: mechanical foreign-handler match arms in the four listed compilers.
- OUT of scope: manifest schema, new handler variants, objective behavior, request facts.
- If any owned handler would fall into the fallback, emit `risk` before committing.

**Implementation:**

- [ ] **Step 1: Run characterization tests before editing**

Run: `scripts/spur-cargo test -p spur-solver --test accessibility_manifest_dispatch --test design_manifest_dispatch --test policy_manifest_dispatch`

Expected: PASS, establishing the current dispatch behavior.

- [ ] **Step 2: Collapse only foreign variants**

```rust
_ => Err(format!("unsupported policy rule `{}`", source.rule_id)),
```

Replace the current combined foreign-variant arm with this fallback. Apply the equivalent family-specific diagnostic to the other three compilers while leaving every owned branch byte-for-byte equivalent.

- [ ] **Step 3: Run characterization tests after editing**

Run: `scripts/spur-cargo test -p spur-solver --test accessibility_manifest_dispatch --test design_manifest_dispatch --test policy_manifest_dispatch`

Run: `scripts/spur-cargo test -p spur-solver rules::families::configuration`

Expected: PASS with identical assertions and diagnostics.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-solver/src/rules/families/accessibility/compile.rs crates/spur-solver/src/rules/families/configuration/compile.rs crates/spur-solver/src/rules/families/design/compile.rs crates/spur-solver/src/rules/families/policy/compile.rs
git commit -m "refactor(spur-solver): task-0a isolate foreign native handlers"
```

---

### Task 0B: Isolate foreign handlers in data, resource, scheduling, and workflow

**Task ID:** `task-0b`

**Files:**
- Modify: `crates/spur-solver/src/rules/families/data_integrity/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/resource/compile.rs:360-470`
- Modify: `crates/spur-solver/src/rules/families/scheduling/compile.rs:712-837`
- Modify: `crates/spur-solver/src/rules/families/workflow/compile.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Each compiler continues to match every handler owned by its family explicitly.
- [ ] Foreign enum growth no longer forces edits in these four unrelated compilers.
- [ ] Existing data-integrity, resource, scheduling, and workflow tests pass without behavior changes.
- [ ] Scheduling's existing typed makespan objective remains exact and complete.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: mechanical foreign-handler match arms in the four listed compilers.
- OUT of scope: manifest schema, objective helper migration, new resource handler branch.
- If any owned handler would fall into the fallback, emit `risk` before committing.

**Implementation:**

- [ ] **Step 1: Run characterization tests before editing**

Run: `scripts/spur-cargo test -p spur-solver --test data_integrity_rule_execution --test resource_manifest_dispatch`

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_execution`

Expected: PASS, including makespan optimum `4` and complete termination.

- [ ] **Step 2: Collapse only foreign variants**

```rust
_ => Err(format!("unsupported resource rule `{}`", source.rule_id)),
```

Replace the current combined foreign-variant arm with this fallback. Apply the equivalent family-specific diagnostic to data integrity, scheduling, and workflow while retaining all owned handler arms.

- [ ] **Step 3: Run characterization tests after editing**

Run: `scripts/spur-cargo test -p spur-solver --test data_integrity_rule_execution --test resource_manifest_dispatch`

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_execution`

Expected: PASS with no solver-envelope changes.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-solver/src/rules/families/data_integrity/compile.rs crates/spur-solver/src/rules/families/resource/compile.rs crates/spur-solver/src/rules/families/scheduling/compile.rs crates/spur-solver/src/rules/families/workflow/compile.rs
git commit -m "refactor(spur-solver): task-0b isolate foreign native handlers"
```

---

### Task 1: Define objective manifest routing and closed handlers

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-solver/src/rules/manifest_format.rs:67-93,225-450,611-790`
- Test: `crates/spur-solver/tests/rule_manifest_format.rs:13-174`
- Test support: `crates/spur-solver/tests/support/mod.rs:39-82`

**Depends on:** `task-0a`, `task-0b`

**Acceptance Criteria:**
- [ ] Omitted `execution_kind` deserializes as `constraint`; serialized catalog data exposes the selected kind.
- [ ] An implemented constraint remains executable only when hard and handler-backed.
- [ ] An implemented objective is executable only when handler-backed and conformance-backed.
- [ ] Objective invalid vectors do not require or advertise verification diagnostics.
- [ ] `RbacMinimumPrivilege` and `PlacementMinimizeSkew` are present once in the stable handler order and have empty parameter ABIs.
- [ ] Existing strict manifest-format tests and the new routing truth table pass.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: strict schema, route truth table, handler family/ABI declarations, manifest-format tests.
- OUT of scope: catalog conversion, family compiler behavior, domain YAML contents.
- If another runtime file is required, emit `scope_drift` before editing it.

**Implementation:**

- [ ] **Step 1: Write failing defaulting and routing tests**

```rust
#[test]
fn execution_kind_defaults_to_constraint_and_objectives_route_explicitly() {
    let source = serde_yml::to_string(&rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    ))
    .unwrap();
    let source = source
        .lines()
        .filter(|line| !line.starts_with("execution_kind:"))
        .collect::<Vec<_>>()
        .join("\n");
    let omitted: RuleManifestV1 = serde_yml::from_str(&source).unwrap();
    assert_eq!(omitted.execution_kind, ExecutionKindV1::Constraint);

    let mut objective = omitted.clone();
    objective.execution_kind = ExecutionKindV1::Objective;
    objective.strength = RuleStrengthV1::Advisory;
    objective.handler = Some(NativeHandlerV1::RbacMinimumPrivilege);
    objective.examples.invalid.expected_diagnostic = None;
    let conformance = objective.conformance.as_mut().unwrap();
    for vector in &mut conformance.invalid {
        vector.expected_diagnostic = None;
    }
    assert_eq!(validate_rule_manifest(&objective), Ok(ManifestRouteV1::Executable));
}
```

- [ ] **Step 2: Run the focused test and confirm the missing-type failure**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_format execution_kind_defaults_to_constraint_and_objectives_route_explicitly -- --nocapture`

Expected: FAIL because `ExecutionKindV1` and the two handler variants do not exist.

- [ ] **Step 3: Add the strict defaulted type and objective-aware route**

```rust
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKindV1 {
    #[default]
    Constraint,
    Objective,
}

#[serde(default)]
pub execution_kind: ExecutionKindV1,

impl RuleManifestV1 {
    #[must_use]
    pub const fn is_executable(&self) -> bool {
        self.availability == AvailabilityV1::Implemented
            && match self.execution_kind {
                ExecutionKindV1::Constraint => self.strength == RuleStrengthV1::Hard,
                ExecutionKindV1::Objective => true,
            }
    }
}
```

Change routing to compare `rule.is_executable()` with handler presence, require conformance for both executable kinds, retain violation-diagnostic validation for constraints, and reject objective example/vector diagnostics so synthesis infeasibility cannot be presented as per-rule verification attribution.

- [ ] **Step 4: Add handlers in family-stable order**

```rust
pub enum NativeHandlerV1 {
    // policy handlers
    RbacMinimumPrivilege,
    // resource handlers
    PlacementMinimizeSkew,
    // remaining handlers
}
```

Map them to `policy` and `resource`, respectively, and return `vec![]` from `parameter_abi`.

- [ ] **Step 5: Run manifest-format verification**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_format`

Expected: PASS with 41 unique handlers and no routing mismatches.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-solver/src/rules/manifest_format.rs crates/spur-solver/tests/rule_manifest_format.rs crates/spur-solver/tests/support/mod.rs
git commit -m "feat(spur-solver): task-1 declare objective manifest routing"
```

---

### Task 2: Project execution kind through runtime binding and catalog guidance

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-solver/src/rules/manifest.rs:36-140,487-550,639-784`
- Modify: `crates/spur-solver/src/rules/catalog.rs:306-370`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] `ManifestRuleContract` and `ValidatedBinding` expose `ExecutionKindV1` selected by the manifest.
- [ ] `validate_binding_contract` accepts every `RuleManifestV1::is_executable()` route and rejects catalog-only rules.
- [ ] Serialized `RuleGuidance` contains `execution_kind` for constraint and objective rules.
- [ ] Existing constraint manifests retain `constraint` without YAML edits.
- [ ] The objective guidance constructor preserves advisory strength, solver encoding, examples, and explicit utility requirements.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: embedded manifest runtime metadata and public catalog projection.
- OUT of scope: handler lowering, request objectives, family facts, frozen fixture regeneration.
- If public MCP response types outside these files need edits, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add failing runtime and guidance tests**

```rust
#[test]
fn objective_guidance_serializes_its_execution_kind() {
    let guidance = RuleGuidance::implemented_objective(
        RuleStrength::Advisory,
        vec![],
        Vec::<String>::new(),
        LlmEncoding::default(),
        SolverEncoding::default(),
        RuleExamples::default(),
    );
    let value = serde_json::to_value(guidance).unwrap();
    assert_eq!(value["execution_kind"], "objective");
    assert_eq!(value["default_strength"], "advisory");
}

#[test]
fn existing_binding_contract_defaults_to_constraint() {
    let contract = manifest_rule_contract("a11y.target_size").unwrap();
    assert_eq!(contract.execution_kind, ExecutionKindV1::Constraint);
}
```

- [ ] **Step 2: Run the focused library test and confirm the missing projection**

Run: `scripts/spur-cargo test -p spur-solver objective_guidance_serializes_its_execution_kind -- --nocapture`

Expected: FAIL because runtime binding and guidance omit `execution_kind`.

- [ ] **Step 3: Carry the manifest enum without deriving semantics twice**

```rust
pub execution_kind: ExecutionKindV1,
```

Add this field to both `ManifestRuleContract` and `ValidatedBinding`. Use `rule.is_executable()` in `validate_binding_contract`; do not duplicate the route truth table in `manifest.rs`.

- [ ] **Step 4: Add catalog classification and constructors**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Constraint,
    Objective,
}

execution_kind: ExecutionKind,
```

Add the field to `RuleGuidance`. Make `implemented_hard` produce `Constraint`, add `implemented_objective(default_strength, ...)`, and let unavailable guidance preserve the manifest kind. Update `convert_rule` to match availability and execution kind while retaining the selected strength.

- [ ] **Step 5: Run library and contract tests**

Run: `scripts/spur-cargo test -p spur-solver rules::manifest::tests`

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_contract`

Expected: PASS; existing catalog-only minimum privilege remains rejected until its domain task changes the manifest.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-solver/src/rules/manifest.rs crates/spur-solver/src/rules/catalog.rs
git commit -m "feat(spur-solver): task-2 expose objective rule metadata"
```

---

### Task 3: Centralize single-minimize request lowering

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-solver/src/rules/primitives.rs:1-134`
- Modify: `crates/spur-solver/src/rules/families/scheduling/compile.rs:1-182,1133-1174`

**Depends on:** `task-0b`

**Acceptance Criteria:**
- [ ] One helper appends exactly one typed `ObjectiveOp::Minimize` under lexicographic priority.
- [ ] A second objective is rejected before `SolveConstraintsRequest::validate` or Z3 invocation.
- [ ] Scheduling uses the helper and retains the exact optimum of four with complete termination.
- [ ] No existing request without an objective changes its solver envelope.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: family-neutral objective append helper and scheduling migration.
- OUT of scope: scheduling manifest classification, new policy/resource behavior, MaxSMT soft constraints.
- If changing solver protocol types appears necessary, emit `risk` and stop.

**Implementation:**

- [ ] **Step 1: Write failing primitive tests**

```rust
#[test]
fn minimize_once_rejects_a_second_objective() {
    let mut request = request("test", vec![], &[], 1_000, false, false);
    push_single_minimize(&mut request, var("cost"), "test.first").unwrap();
    let error = push_single_minimize(&mut request, var("other"), "test.second")
        .expect_err("one objective per request");
    assert_eq!(request.objectives.len(), 1);
    assert!(error.contains("at most one objective binding"));
}
```

- [ ] **Step 2: Run the focused test and confirm the helper is absent**

Run: `scripts/spur-cargo test -p spur-solver rules::primitives::tests::minimize_once_rejects_a_second_objective -- --nocapture`

Expected: FAIL because `push_single_minimize` is not defined.

- [ ] **Step 3: Implement one typed append point**

```rust
pub fn push_single_minimize(
    request: &mut SolveConstraintsRequest,
    expr: ConstraintExpr,
    rule_id: &str,
) -> Result<(), String> {
    if !request.objectives.is_empty() {
        return Err(format!(
            "at most one objective binding is allowed; `{rule_id}` would add another"
        ));
    }
    request.objectives.push(Objective { op: ObjectiveOp::Minimize, expr });
    Ok(())
}
```

Do not add maximize, priority selection, solution enumeration, or user-authored expressions.

- [ ] **Step 4: Migrate scheduling without changing its dual verify/synthesize semantics**

```rust
if input.mode == RuleSolveMode::Synthesize && makespan_bindings == 1 {
    push_single_minimize(
        &mut solver_request,
        var(resolver.makespan_variable.as_ref().unwrap()),
        "scheduling.minimize_makespan",
    )?;
}
```

- [ ] **Step 5: Run primitive and scheduling tests**

Run: `scripts/spur-cargo test -p spur-solver rules::primitives::tests`

Run: `scripts/spur-cargo test -p spur-solver rules::families::scheduling::compile::tests::synthesis_uses_one_typed_objective_and_decodes_optimum_four`

Expected: PASS; exact objective bound remains finite `4`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-solver/src/rules/primitives.rs crates/spur-solver/src/rules/families/scheduling/compile.rs
git commit -m "refactor(spur-solver): task-3 centralize single minimize lowering"
```

---

### Task 4: Implement `rbac.minimum_privilege`

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-solver/src/rules/families/policy/compile.rs:1-869`
- Modify: `crates/spur-solver/src/rules/families/policy/rules/minimum_privilege.yaml`
- Create: `crates/spur-solver/tests/policy_objective_execution.rs`

**Depends on:** `task-2`, `task-3`

**Acceptance Criteria:**
- [ ] Each scoped principal declares non-empty, duplicate-free `required_permissions` and positive `grant_costs` keyed by known roles.
- [ ] Every scoped candidate `principal_role` unknown has a cost; at least one scoped candidate exists.
- [ ] Every required permission has a matching hard `rbac.permission_reachable` binding for that principal.
- [ ] Verify mode, missing coverage, missing costs, non-positive costs, unknown roles/principals, and duplicate objective bindings fail before solving.
- [ ] Synthesis minimizes candidate grant cost while reachability, hierarchy, session authorization, and separation-of-duty predicates remain hard.
- [ ] The canonical fixture terminates completely with exact optimum cost `3`; the strict-better bound `<= 2` is unsatisfiable.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: policy facts/schema, objective validation/lowering, policy objective manifest and focused execution tests.
- OUT of scope: session-role utility, inferred permission requirements, generic preferences, policy catalog snapshot tests.
- If utility cannot be represented as positive finite integers over existing candidate variables, emit `risk` before changing the request ABI.

**Implementation:**

- [ ] **Step 1: Add a failing exact-optimum integration test**

```rust
fn minimum_privilege_request() -> Value {
    json!({
        "family": "policy",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "rbac.minimum_privilege", "subjects": ["alice"], "parameters": {}},
            {"rule_id": "rbac.permission_reachable", "subjects": ["alice", "read"], "parameters": {}},
            {"rule_id": "rbac.permission_reachable", "subjects": ["alice", "write"], "parameters": {}}
        ],
        "facts": {
            "roles": {
                "reader": {"inherits": [], "permissions": ["read"]},
                "writer": {"inherits": [], "permissions": ["write"]},
                "admin": {"inherits": [], "permissions": ["read", "write"]}
            },
            "principals": {
                "alice": {
                    "roles": [],
                    "required_permissions": ["read", "write"],
                    "grant_costs": {"reader": 1, "writer": 2, "admin": 5}
                }
            },
            "sessions": {}
        },
        "unknowns": [
            {"kind": "principal_role", "principal": "alice", "role": "reader"},
            {"kind": "principal_role", "principal": "alice", "role": "writer"},
            {"kind": "principal_role", "principal": "alice", "role": "admin"}
        ]
    })
}

#[tokio::test]
async fn minimum_privilege_proves_cost_three() {
    let result = run(&SolverService::new(), prepare(minimum_privilege_request()).unwrap())
        .await
        .unwrap();
    let optimization = result.solver.optimization.unwrap();
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(optimization.solutions[0].objectives[0].value, Some(ModelValue::Int(3)));
    assert_eq!(
        optimization.solutions[0].objectives[0].bound,
        ObjectiveBound::Finite { exact: "3".to_owned() }
    );
}
```

The request must include reader cost `1`, writer cost `2`, admin cost `5`, required permissions `read` and `write`, three candidate role unknowns, and hard reachability bindings for both permissions.

- [ ] **Step 2: Run the focused test and confirm catalog-only rejection**

Run: `scripts/spur-cargo test -p spur-solver --test policy_objective_execution minimum_privilege_proves_cost_three -- --nocapture`

Expected: FAIL because the manifest has no handler and the compiler has no objective branch.

- [ ] **Step 3: Extend policy facts and validate caller-owned utility**

```rust
struct PrincipalFacts {
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    required_permissions: Vec<String>,
    #[serde(default)]
    grant_costs: BTreeMap<String, i64>,
}
```

Validate positive costs, role existence, duplicate permissions, scoped candidate coverage, and exact `(principal, permission)` reachability bindings. Count bindings whose validated execution kind is `Objective`; reject count greater than one and reject any objective in verify mode.

- [ ] **Step 4: Keep the objective binding hard-neutral and append its cost expression**

```rust
fn minimum_privilege_cost(
    subjects: &[String],
    resolver: &PolicyResolver,
) -> Result<ConstraintExpr, String> {
    let mut terms = Vec::new();
    for principal in subjects {
        for ((candidate_principal, role), variable) in &resolver.principal_unknowns {
            if candidate_principal == principal {
                let cost = resolver.facts.principals[principal].grant_costs[role];
                terms.push(mul(vec![int(cost), var(variable.clone())]));
            }
        }
    }
    Ok(sum(terms))
}
```

Return `boolean(true)` for the objective binding's compiled predicate; required reachability and every other policy binding remain named hard constraints. Call `push_single_minimize` only after the hard request is constructed.

- [ ] **Step 5: Make the manifest executable and synthesis-only by contract**

Set `availability: implemented`, `execution_kind: objective`, retain `strength: advisory`, set `subjects` to `at_least: 1`, add `handler: rbac_minimum_privilege`, and provide one feasible synthesis vector plus one hard-infeasible synthesis vector without expected verification diagnostics.

- [ ] **Step 6: Add rejection and ratchet tests**

```rust
#[test]
fn minimum_privilege_rejects_verify_missing_cost_and_reachability() {
    let mut verify = minimum_privilege_request();
    verify["mode"] = json!("verify");

    let mut missing_cost = minimum_privilege_request();
    missing_cost["facts"]["principals"]["alice"]["grant_costs"]
        .as_object_mut().unwrap().remove("writer");

    let mut uncovered = minimum_privilege_request();
    uncovered["rules"].as_array_mut().unwrap()
        .retain(|rule| rule["subjects"] != json!(["alice", "write"]));

    let mut duplicate = minimum_privilege_request();
    let objective = duplicate["rules"][0].clone();
    duplicate["rules"].as_array_mut().unwrap().push(objective);

    for request in [verify, missing_cost, uncovered, duplicate] {
        assert!(prepare(request).is_err());
    }
}

#[tokio::test]
async fn cost_below_three_is_unsatisfiable() {
    let mut prepared = prepare(minimum_privilege_request()).unwrap();
    let objective = prepared.request.objectives[0].expr.clone();
    prepared.request.constraints.push(ConstraintItem::Declared(ConstraintDecl {
        id: Some("test.strict_better".to_owned()),
        group: None,
        soft: false,
        weight: None,
        expr: le(objective, int(2)),
    }));
    assert_eq!(SolverService::new().solve_constraints(prepared.request).await.unwrap().status,
               SolveStatus::Unsat);
}
```

- [ ] **Step 7: Run policy objective tests**

Run: `scripts/spur-cargo test -p spur-solver --test policy_objective_execution`

Expected: PASS with one finite objective, complete termination, and strict-better unsat.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-solver/src/rules/families/policy/compile.rs crates/spur-solver/src/rules/families/policy/rules/minimum_privilege.yaml crates/spur-solver/tests/policy_objective_execution.rs
git commit -m "feat(spur-solver): task-4 execute minimum privilege objective"
```

---

### Task 5: Ratchet policy manifest ownership and dispatch

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-solver/tests/policy_rule_manifests.rs:1-158`
- Modify: `crates/spur-solver/tests/policy_manifest_dispatch.rs:1-124`

**Depends on:** `task-4`

**Acceptance Criteria:**
- [ ] Policy manifests expose five executable routes and five unique handlers.
- [ ] Minimum privilege is implemented, advisory, objective-classified, handler-backed, and conformance-backed.
- [ ] Native dispatch produces one hard-neutral compiled binding plus one typed minimize objective.
- [ ] The former catalog-only assertions are removed and replaced by executable-route assertions.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: policy manifest ownership and dispatch tests.
- OUT of scope: compiler implementation, resource files, global counts and snapshots.
- If a production edit is needed, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Replace the stale catalog-only expectations**

```rust
#[test]
fn minimum_privilege_is_an_executable_objective_route() {
    let rule = policy_manifests().1.into_iter()
        .find(|rule| rule.id == "rbac.minimum_privilege").unwrap();
    assert_eq!(rule.availability, AvailabilityV1::Implemented);
    assert_eq!(rule.execution_kind, ExecutionKindV1::Objective);
    assert_eq!(rule.handler, Some(NativeHandlerV1::RbacMinimumPrivilege));
    assert_eq!(validate_rule_manifest(&rule), Ok(ManifestRouteV1::Executable));
}
```

- [ ] **Step 2: Add minimum privilege to the exact handler map and dispatch matrix**

Use a synthesis request containing explicit utility and reachability bindings. Assert `prepared.request.objectives.len() == 1`, `ObjectiveOp::Minimize`, and no soft constraints.

- [ ] **Step 3: Run policy manifest tests**

Run: `scripts/spur-cargo test -p spur-solver --test policy_rule_manifests --test policy_manifest_dispatch`

Expected: PASS; all five policy rule IDs route through closed handlers.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-solver/tests/policy_rule_manifests.rs crates/spur-solver/tests/policy_manifest_dispatch.rs
git commit -m "test(spur-solver): task-5 ratchet policy objective dispatch"
```

---

### Task 6: Implement `placement.minimize_skew`

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-solver/src/rules/families/resource/compile.rs:1-917`
- Create: `crates/spur-solver/src/rules/families/resource/rules/minimize_skew.yaml`
- Create: `crates/spur-solver/tests/resource_objective_execution.rs`

**Depends on:** `task-2`, `task-3`

**Acceptance Criteria:**
- [ ] The objective requires exactly one workload, at least two declared domains, a known or bounded-unknown replica count, and at least one bounded domain-count unknown.
- [ ] Domain counts are nonnegative, sum to replicas, and constrain one internal `skew` above both directions of every unordered pair difference.
- [ ] Internal skew is bounded `0..=replica_upper_bound`, minimized once, and projected as `topology_skew`.
- [ ] Existing capacity, quota, request-limit, minimum-domain, and topology-cap bindings remain named hard predicates.
- [ ] Verify, missing bounds, duplicate objective, inconsistent conservation, and fewer-than-two-domain inputs fail or prove infeasible as specified.
- [ ] The canonical three-replica/two-domain fixture terminates completely with exact optimum skew `1`; an even four-replica fixture proves `0`.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: resource compiler, new objective manifest, and focused resource objective tests.
- OUT of scope: Kubernetes scheduler emulation, implicit domains, affinity/taints, global catalog fixtures.
- If a live-cluster fact source appears necessary, emit `risk` and stop.

**Implementation:**

- [ ] **Step 1: Write the failing exact-skew test**

```rust
fn minimize_skew_request(replicas: i64) -> Value {
    json!({
        "family": "resource",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "placement.minimize_skew", "subjects": ["api"], "parameters": {}}
        ],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": replicas,
                    "requests": {},
                    "limits": {},
                    "domain_counts": {"zone-a": null, "zone-b": null}
                }
            },
            "pools": {},
            "quotas": {}
        },
        "unknowns": [
            {"subject": "api", "field": "domain_counts.zone-a", "min": 0, "max": replicas},
            {"subject": "api", "field": "domain_counts.zone-b", "min": 0, "max": replicas}
        ]
    })
}

#[tokio::test]
async fn minimize_skew_proves_one_for_three_replicas() {
    let result = run(&SolverService::new(), prepare(minimize_skew_request(3)).unwrap())
        .await
        .unwrap();
    let optimization = result.solver.optimization.unwrap();
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(optimization.solutions[0].objectives[0].value, Some(ModelValue::Int(1)));
    assert!(result.assignments.iter().any(|item|
        item.field == "topology_skew" && item.value == ModelValue::Int(1)));
}
```

- [ ] **Step 2: Run the focused test and confirm the unknown rule failure**

Run: `scripts/spur-cargo test -p spur-solver --test resource_objective_execution minimize_skew_proves_one_for_three_replicas -- --nocapture`

Expected: FAIL because the rule manifest and handler do not exist.

- [ ] **Step 3: Add bounded internal skew state**

```rust
fn add_skew_variable(
    resolver: &mut ResourceResolver,
    workload: &str,
    replica_upper_bound: i64,
) -> Result<String, String> {
    let name = format!("resource_skew_{}", resolver.variables.len());
    resolver.variables.push(Variable::IntRange {
        name: name.clone(), min: 0, max: replica_upper_bound,
    });
    resolver.projections.push(ModelProjection {
        variable: name.clone(),
        subject: workload.to_owned(),
        field: "topology_skew".to_owned(),
    });
    Ok(name)
}
```

Derive the upper bound from fixed replicas or the declared `replicas` unknown maximum. Reject a null replica count without that exact unknown.

- [ ] **Step 4: Compile conservation and pairwise bounds as hard predicates**

```rust
for (index, left) in counts.iter().enumerate() {
    for right in &counts[index + 1..] {
        predicates.push(ge(var(&skew), sub(left.clone(), right.clone())));
        predicates.push(ge(var(&skew), sub(right.clone(), left.clone())));
    }
}
predicates.push(eq(sum(counts), replicas));
```

Return the conjunction from the objective binding so conservation and bounds are named hard constraints. Append `minimize skew` only in synthesis through `push_single_minimize`.

- [ ] **Step 5: Author the strict manifest**

Use `execution_kind: objective`, `availability: implemented`, `strength: advisory`, profile `topology_placement`, exact one subject, handler `placement_minimize_skew`, no parameters, a complete optimum vector, and a conservation-infeasible vector without verification diagnostics.

- [ ] **Step 6: Add rejection, zero-optimum, and ratchet tests**

```rust
#[test]
fn minimize_skew_rejects_verify_unbounded_and_duplicate_objectives() {
    let mut verify = minimize_skew_request(3);
    verify["mode"] = json!("verify");

    let mut unbounded = minimize_skew_request(3);
    unbounded["unknowns"].as_array_mut().unwrap().pop();

    let mut duplicate = minimize_skew_request(3);
    let objective = duplicate["rules"][0].clone();
    duplicate["rules"].as_array_mut().unwrap().push(objective);

    for request in [verify, unbounded, duplicate] {
        assert!(prepare(request).is_err());
    }
}

#[tokio::test]
async fn even_replicas_have_zero_optimum_and_negative_skew_is_unsat() {
    let prepared = prepare(minimize_skew_request(4)).unwrap();
    let result = run(&SolverService::new(), prepared).await.unwrap();
    let optimization = result.solver.optimization.unwrap();
    assert_eq!(optimization.solutions[0].objectives[0].value, Some(ModelValue::Int(0)));

    let mut strict = prepare(minimize_skew_request(4)).unwrap();
    let objective = strict.request.objectives[0].expr.clone();
    strict.request.constraints.push(ConstraintItem::Declared(ConstraintDecl {
        id: Some("test.strict_better".to_owned()),
        group: None,
        soft: false,
        weight: None,
        expr: le(objective, int(-1)),
    }));
    let strict = SolverService::new().solve_constraints(strict.request).await.unwrap();
    assert_eq!(strict.status, SolveStatus::Unsat);
}
```

- [ ] **Step 7: Run resource objective tests**

Run: `scripts/spur-cargo test -p spur-solver --test resource_objective_execution`

Expected: PASS with exact complete bounds `1` and `0`, deterministic derived projection, and rejection before solving.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-solver/src/rules/families/resource/compile.rs crates/spur-solver/src/rules/families/resource/rules/minimize_skew.yaml crates/spur-solver/tests/resource_objective_execution.rs
git commit -m "feat(spur-solver): task-6 add placement skew objective"
```

---

### Task 7: Ratchet resource manifest ownership and dispatch

**Task ID:** `task-7`

**Files:**
- Modify: `crates/spur-solver/tests/resource_rule_manifests.rs:1-199`
- Modify: `crates/spur-solver/tests/resource_manifest_dispatch.rs:1-115`

**Depends on:** `task-6`

**Acceptance Criteria:**
- [ ] Resource ownership contains six rules, including three topology-placement rules.
- [ ] Six resource handlers are unique and exactly mapped to their manifests.
- [ ] Minimize-skew dispatch produces its hard predicate and one minimize objective.
- [ ] Existing five resource rules preserve their handler, formula, and dispatch assertions.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: resource manifest and native dispatch tests.
- OUT of scope: production compiler code, policy tests, global frozen fixture.
- If a production edit is needed, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Extend the exact file and ownership sets**

```rust
const RULE_FILES: [&str; 6] = [
    "aggregate_capacity.yaml",
    "minimum_failure_domains.yaml",
    "minimize_skew.yaml",
    "quota_capacity.yaml",
    "request_within_limit.yaml",
    "topology_max_skew.yaml",
];
```

Add `placement.minimize_skew` to the `topology_placement` owner set.

- [ ] **Step 2: Add objective dispatch assertions**

Compile a synthesis request with bounded `domain_counts.zone-a` and `domain_counts.zone-b`. Assert the prepared request has one named hard constraint, one minimize objective, lex priority, and a projection with field `topology_skew`.

- [ ] **Step 3: Run resource manifest tests**

Run: `scripts/spur-cargo test -p spur-solver --test resource_rule_manifests --test resource_manifest_dispatch`

Expected: PASS with six manifest/handler pairs.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-solver/tests/resource_rule_manifests.rs crates/spur-solver/tests/resource_manifest_dispatch.rs
git commit -m "test(spur-solver): task-7 ratchet resource objective dispatch"
```

---

### Task 8: Extend global catalog and conformance coverage

**Task ID:** `task-8`

**Files:**
- Modify: `crates/spur-solver/tests/platform_rule_catalog.rs:1-306`
- Modify: `crates/spur-solver/tests/platform_rule_conformance.rs:1-210`
- Modify: `crates/spur-solver/tests/rule_manifest_loader.rs:1-105`

**Depends on:** `task-5`, `task-7`

**Acceptance Criteria:**
- [ ] Global catalog tests assert 41 sorted catalog rules and 41 sorted executable rules.
- [ ] Minimum privilege is no longer listed as capability-unavailable; placement skew is discoverable under `topology_placement`.
- [ ] Conformance covers 41 unique handlers exactly once.
- [ ] Valid objective vectors require one finite objective bound and `termination = complete`.
- [ ] Invalid objective synthesis vectors require `infeasible` and empty per-rule attribution.
- [ ] Existing constraint vectors retain their pass/fail behavior and diagnostics.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: global ID sets, catalog discovery assertions, generic conformance semantics.
- OUT of scope: frozen JSON fixture, MCP count tests, compiler implementation.
- If a domain-specific assertion is needed, place it in the domain test instead of branching the generic harness.

**Implementation:**

- [ ] **Step 1: Change the exact global expectations**

```rust
const EXPECTED_EXECUTABLE_RULE_IDS: [&str; 41] = [
    // existing sorted IDs
    "placement.minimize_skew",
    // existing placement/resource IDs
    "rbac.minimum_privilege",
    // remaining sorted IDs
];
```

Assert the embedded manifest registry contains 41 rules and that the policy executable projection contains all five policy IDs.

- [ ] **Step 2: Make conformance execution-kind aware**

```rust
if case.execution_kind == ExecutionKindV1::Objective {
    let optimization = result.solver.optimization.as_ref().expect("objective metadata");
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(optimization.solutions.len(), 1);
    assert_eq!(optimization.solutions[0].objectives.len(), 1);
    assert!(matches!(optimization.solutions[0].objectives[0].bound,
                     ObjectiveBound::Finite { .. }));
}
```

Obtain execution kind from `manifest_rule_contract`; do not infer it from handler names.

- [ ] **Step 3: Preserve synthesis infeasibility semantics**

For every invalid objective vector, assert `SolveStatus::Unsat`, `RuleOutcome::Infeasible`, and `rule_results.is_empty()`.

- [ ] **Step 4: Run global catalog and conformance tests**

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_catalog --test platform_rule_conformance --test rule_manifest_loader`

Expected: PASS with 41/41 coverage and no duplicate handler.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/tests/platform_rule_catalog.rs crates/spur-solver/tests/platform_rule_conformance.rs crates/spur-solver/tests/rule_manifest_loader.rs
git commit -m "test(spur-solver): task-8 enforce objective catalog conformance"
```

---

### Task 9: Refresh the frozen catalog and MCP count ratchets

**Task ID:** `task-9`

**Files:**
- Modify: `crates/spur-solver/tests/fixtures/builtin_rule_catalog_v1.json`
- Modify: `crates/spur-solver/tests/rule_manifest_equivalence.rs:1-55`
- Modify: `crates/spur-solver/tests/solve_rules_mcp.rs:160-200`

**Depends on:** `task-5`, `task-7`

**Acceptance Criteria:**
- [ ] Frozen catalog JSON exactly equals the embedded registry with `execution_kind` projected.
- [ ] Fixture contains 41 sorted rules and both objective IDs.
- [ ] `solve_rules` schema exposes 41 executable rule IDs in stable order.
- [ ] No hand-edited ordering drift or unrelated fixture formatting changes are introduced.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: frozen public catalog and exact MCP enum/count assertions.
- OUT of scope: production Rust, conformance execution, solver behavior.
- If the generated catalog differs outside the new field/rules, emit `risk` with the diff summary.

**Implementation:**

- [ ] **Step 1: Run the frozen-catalog test and capture the exact diff**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence builtin_registry_matches_frozen_catalog_v1 -- --nocapture`

Expected: FAIL showing the added public `execution_kind`, activated minimum-privilege guidance, and new placement rule.

- [ ] **Step 2: Regenerate the fixture from the serialized `manifest_registry()` value**

Use the repository's existing fixture-generation path if present; otherwise add a temporary ignored test that prints `serde_json::to_string_pretty(manifest_registry())`, capture its output, update only the fixture through `apply_patch`, then remove the temporary test before commit.

- [ ] **Step 3: Ratchet exact rule counts and IDs**

```rust
assert_eq!(rule_ids.len(), 41);
assert!(rule_ids.contains(&"rbac.minimum_privilege"));
assert!(rule_ids.contains(&"placement.minimize_skew"));
```

Update the MCP schema enum assertion from 39 to 41 executable IDs.

- [ ] **Step 4: Run snapshot and MCP tests**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence --test solve_rules_mcp`

Expected: PASS with byte-stable sorted fixture data.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/tests/fixtures/builtin_rule_catalog_v1.json crates/spur-solver/tests/rule_manifest_equivalence.rs crates/spur-solver/tests/solve_rules_mcp.rs
git commit -m "test(spur-solver): task-9 refresh objective catalog snapshot"
```

---

### Task 10: Enforce the domain-objective release gate

**Task ID:** `task-10`

**Files:**
- Create: `crates/spur-solver/tests/domain_objective_release.rs`

**Depends on:** `task-4`, `task-6`

**Acceptance Criteria:**
- [ ] Both objective rules prove their documented finite optimum with complete termination.
- [ ] Adding a strict-better hard bound to each compiled typed request proves unsatisfiable.
- [ ] Verify-mode and duplicate-objective requests fail during `prepare`.
- [ ] Hard-infeasible synthesis returns `infeasible` with no verification attribution.
- [ ] A 32-domain bounded placement fixture completes within its request timeout and returns one finite bound.
- [ ] The full `spur-solver` test suite and formatting checks pass.

**Selected Worker:** `codex` (`gpt-5.6-sol`, `xhigh`; explicit user override)

**Scope Boundary:**
- IN scope: one release-gate integration test file and crate-wide verification.
- OUT of scope: production fixes beyond one-line testability adjustments; emit `scope_drift` for substantive production changes.
- If timeout or unknown occurs, emit `risk` with the request size and solver termination metadata.

**Implementation:**

- [ ] **Step 1: Write the release matrix before running it**

```rust
#[tokio::test]
async fn objective_release_matrix_is_complete_and_ratcheted() {
    for (rule_id, expected) in [
        ("rbac.minimum_privilege", 3_i64),
        ("placement.minimize_skew", 1_i64),
    ] {
        let request = manifest_conformance_vectors(rule_id).unwrap().valid[0].request.clone();
        let prepared = prepare(request.clone()).unwrap();
        assert_eq!(prepared.request.objectives.len(), 1);
        let result = run(&SolverService::new(), prepared).await.unwrap();
        let optimization = result.solver.optimization.unwrap();
        assert_eq!(optimization.termination, OptimizationTermination::Complete);
        assert_eq!(optimization.solutions[0].objectives[0].value, Some(ModelValue::Int(expected)));
        assert_eq!(
            optimization.solutions[0].objectives[0].bound,
            ObjectiveBound::Finite { exact: expected.to_string() },
        );

        let mut strict = prepare(request).unwrap();
        let expr = strict.request.objectives[0].expr.clone();
        strict.request.constraints.push(ConstraintItem::Declared(ConstraintDecl {
            id: Some(format!("{rule_id}.strict_better")),
            group: None,
            soft: false,
            weight: None,
            expr: le(expr, int(expected - 1)),
        }));
        let strict = SolverService::new().solve_constraints(strict.request).await.unwrap();
        assert_eq!(strict.status, SolveStatus::Unsat, "{rule_id}");
    }
}
```

- [ ] **Step 2: Add rejection and infeasibility assertions**

```rust
#[test]
fn objective_preflight_rejects_verify_and_duplicates() {
    for rule_id in ["rbac.minimum_privilege", "placement.minimize_skew"] {
        let base = manifest_conformance_vectors(rule_id).unwrap().valid[0].request.clone();

        let mut verify = base.clone();
        verify["mode"] = json!("verify");
        assert!(prepare(verify).is_err(), "{rule_id} verify");

        let mut duplicate = base;
        let binding = duplicate["rules"].as_array().unwrap().iter()
            .find(|binding| binding["rule_id"] == rule_id).unwrap().clone();
        duplicate["rules"].as_array_mut().unwrap().push(binding);
        assert!(prepare(duplicate).is_err(), "{rule_id} duplicate");
    }
}

#[tokio::test]
async fn hard_infeasibility_has_no_rule_attribution() {
    for rule_id in ["rbac.minimum_privilege", "placement.minimize_skew"] {
        let request = manifest_conformance_vectors(rule_id).unwrap().invalid[0].request.clone();
        let result = run(&SolverService::new(), prepare(request).unwrap()).await.unwrap();
        assert_eq!(result.outcome, RuleOutcome::Infeasible);
        assert!(result.rule_results.is_empty());
    }
}
```

- [ ] **Step 3: Add the bounded near-limit fixture**

```rust
fn near_limit_resource_request() -> Value {
    let mut domain_counts = Map::new();
    let mut unknowns = Vec::new();
    for index in 0..32 {
        let domain = format!("zone-{index:02}");
        domain_counts.insert(domain.clone(), Value::Null);
        unknowns.push(json!({
            "subject": "api",
            "field": format!("domain_counts.{domain}"),
            "min": 0,
            "max": 64
        }));
    }
    json!({
        "family": "resource",
        "mode": "synthesize",
        "rules": [{"rule_id": "placement.minimize_skew", "subjects": ["api"], "parameters": {}}],
        "facts": {
            "workloads": {"api": {
                "replicas": 64,
                "requests": {},
                "limits": {},
                "domain_counts": domain_counts
            }},
            "pools": {},
            "quotas": {}
        },
        "unknowns": unknowns,
        "timeout_ms": 30_000
    })
}
```

Run this request and assert complete termination, one solution, and one finite objective bound; do not assert a tied domain assignment.

- [ ] **Step 4: Run the release test**

Run: `scripts/spur-cargo test -p spur-solver --test domain_objective_release -- --nocapture`

Expected: PASS with zero unknown/time-limit outcomes.

- [ ] **Step 5: Run full crate verification and formatting**

Run: `scripts/spur-cargo test -p spur-solver`

Run: `scripts/spur-cargo fmt --all -- --check`

Expected: all `spur-solver` tests pass; formatting exits zero.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-solver/tests/domain_objective_release.rs
git commit -m "test(spur-solver): task-10 enforce objective release gate"
```

---

## Self-review

### Spec coverage

- Reusable objective contract and closed-handler seam: Tasks 0A-3.
- Caller-owned minimum-privilege utility and hard reachability: Tasks 4-5.
- Bounded placement skew, conservation, and projection: Tasks 6-7.
- Complete termination, finite bounds, infeasibility, and ratcheted strict-better proofs: Tasks 8 and 10.
- Catalog discoverability, exact counts, MCP schema, and frozen snapshot: Tasks 8-9.
- Existing hard-rule semantics and no multi-objective expansion: enforced by Tasks 1, 3, and 10.

### Type consistency

- Manifest layer uses `ExecutionKindV1`; public catalog uses `ExecutionKind`.
- `ValidatedBinding.execution_kind` is the only family compiler classification source.
- Every objective-producing compiler appends through `push_single_minimize`.
- Policy objective values are integer grant costs; resource objective values are integer skew.
- Derived resource projection uses existing `ModelProjection` and wire field `topology_skew`.

### DAG validation

- No task transitively depends on itself.
- Tasks 0A and 0B are disjoint parallel roots; Task 3 follows 0B because both touch scheduling.
- Task 1 waits for both foreign-handler refactors before extending the closed enum.
- Policy Tasks 4-5 and resource Tasks 6-7 have disjoint writes and parallelize after shared interfaces.
- Global Tasks 8-10 write disjoint test files and parallelize after domain completion.

### Release boundary

- One objective binding per request.
- Lexicographic priority remains fixed.
- One collected solution remains fixed.
- Both new objectives are synthesis-only.
- Scheduling retains its existing dual-mode behavior.
- Pareto/box enumeration, inferred utility, MaxSMT repair, and additional families remain outside Wave 1.
