# Data Integrity Solver Rule Family Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-21-data-integrity-solver-family-design.ipynb`
**Formal @spec cells:** `DATA-INTEGRITY-CELL-MODES`, `DATA-INTEGRITY-FINITE-INSTANCE`, `DATA-INTEGRITY-RELEASE-GATE`
**Design epic:** `bd-1usc` (closed)

**Goal:** Add a discoverable, manifest-backed `data_integrity` solver family that verifies and synthesizes bounded relational snapshots for eight strict integrity rules.

**Architecture:** The family uses one `finite_relational_snapshot` profile and a strict Rust compiler that normalizes bounded relations, fields, rows, cells, and explicit unknown declarations into the existing typed `SolveConstraintsRequest`. YAML owns stable catalog metadata and valid/invalid conformance vectors; Rust owns reference validation, typed variable allocation, checked AST budgeting, eight native lowerings, deterministic attribution, and caller-order projection.

**Tech Stack:** Rust 2021, Serde/serde_json, strict v1 YAML manifests, existing `ConstraintExpr`/Z3 execution path, `scripts/spur-cargo` for all Rust build and test commands.

---

### Task 1: Extend the closed native handler ABI

**Task ID:** `handler-abi`

**Files:**

- Modify: `crates/spur-solver/src/rules/manifest_format.rs:225-418`
- Modify: `crates/spur-solver/tests/rule_manifest_format.rs:27-90`

**Depends on:** none

**Acceptance Criteria:**

- [ ] `NativeHandlerV1` contains exactly eight new `DataIntegrity*` variants.
- [ ] Existing handler order remains unchanged; new handlers occupy indices `31..39` in the approved rule order.
- [ ] Every new handler maps to family `data_integrity` and has an empty parameter ABI.
- [ ] Closed-enum and stable-order tests pass with 39 handlers.

**Suggested Worker:** codex — two-file, mechanical closed-enum change.

**Scope Boundary:**

- IN scope: handler enum variants, `ALL`, `family()`, `parameter_abi()`, and their format tests.
- OUT of scope: manifests, compiler registration, fact models, constraint lowering, catalog snapshots.
- If any rule needs a caller parameter rather than a fact-definition subject, emit `scope_drift`; the approved ABI is empty for all eight handlers.

**Implementation:**

- [ ] **Step 1: Write the failing stable-order test**

```rust
#[test]
fn data_integrity_handlers_follow_workflow_in_stable_order() {
    let expected = [
        NativeHandlerV1::DataIntegrityUnique,
        NativeHandlerV1::DataIntegrityForeignKey,
        NativeHandlerV1::DataIntegrityCardinality,
        NativeHandlerV1::DataIntegrityValueRange,
        NativeHandlerV1::DataIntegrityConditionalRequired,
        NativeHandlerV1::DataIntegrityAggregateBalance,
        NativeHandlerV1::DataIntegrityMutuallyConsistent,
        NativeHandlerV1::DataIntegrityTemporalConsistency,
    ];
    assert_eq!(NativeHandlerV1::ALL.get(31..39), Some(expected.as_slice()));
}
```

- [ ] **Step 2: Run RED**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_format data_integrity_handlers_follow_workflow_in_stable_order -- --nocapture`

Expected: compilation fails because the eight variants do not exist.

- [ ] **Step 3: Implement the exact closed ABI**

Append the eight variants after `WorkflowBoundedReachability`, append them to `ALL`, map all eight to `"data_integrity"`, and include them in the empty-vector arm of `parameter_abi()`.

- [ ] **Step 4: Update the closed-enum assertions and run GREEN**

Change both handler-count assertions from 31 to 39 and assert serialization includes `data_integrity_temporal_consistency`.

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_format`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/src/rules/manifest_format.rs crates/spur-solver/tests/rule_manifest_format.rs
git commit -m "feat(spur-solver): handler-abi add data integrity handlers"
```

---

### Task 2: Add the complete data-integrity manifest bundle

**Task ID:** `manifest-bundle`

**Files:**

- Create: `crates/spur-solver/src/rules/families/data_integrity/family.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/unique.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/foreign_key.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/cardinality.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/value_range.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/conditional_required.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/aggregate_balance.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/mutually_consistent.yaml`
- Create: `crates/spur-solver/src/rules/families/data_integrity/rules/temporal_consistency.yaml`
- Modify: `crates/spur-solver/tests/rule_manifest_build.rs:117-175`

**Depends on:** `handler-abi`

**Acceptance Criteria:**

- [ ] The manifest loader discovers family `data_integrity`, profile `finite_relational_snapshot`, and all eight rules in stable ID order.
- [ ] Every rule is implemented-hard, binds exactly one definition ID, declares no caller parameters, and selects its corresponding closed handler.
- [ ] Every rule manifest contains one complete valid vector and one complete invalid vector with exact `<rule_id>.violation` diagnostic.
- [ ] Authorities, formulas, null behavior, synthesis bounds, and anti-patterns match the approved notebook.
- [ ] Manifest build and validation tests pass.

**Suggested Worker:** claude-code — coordinated multi-file authoring from an approved specification.

**Scope Boundary:**

- IN scope: family/profile metadata, eight rule manifests, conformance request fixtures, manifest discovery test.
- OUT of scope: Rust compiler code, registry compiler array, frozen catalog fixture, public result projection.
- This task is allowed to exceed five files because each additional file is one small declarative rule document and the bundle validates atomically.

**Implementation:**

- [ ] **Step 1: Add the failing catalog-order test**

```rust
#[test]
fn data_integrity_catalog_uses_approved_profile_and_rule_order() {
    let loaded = load_manifest_sources(&repository_manifest_root()).unwrap();
    let family = loaded.bundle.families.iter()
        .find(|family| family.id == "data_integrity").unwrap();
    assert_eq!(family.profiles[0].id, "finite_relational_snapshot");
    let ids = loaded.bundle.rules.iter()
        .filter(|rule| rule.family == "data_integrity")
        .map(|rule| rule.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, [
        "data_integrity.aggregate_balance",
        "data_integrity.cardinality",
        "data_integrity.conditional_required",
        "data_integrity.foreign_key",
        "data_integrity.mutually_consistent",
        "data_integrity.temporal_consistency",
        "data_integrity.unique",
        "data_integrity.value_range",
    ]);
}
```

- [ ] **Step 2: Run RED**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_build data_integrity_catalog_uses_approved_profile_and_rule_order -- --nocapture`

Expected: the data-integrity family is absent.

- [ ] **Step 3: Author the strict bundle**

Use `subjects.cardinality = {kind: exact, count: 1}`, `parameters: []`, and these handlers:

```yaml
handler: data_integrity_unique
```

with the corresponding suffix for each rule. Encode SQL-like NULLS DISTINCT for `unique`, MATCH SIMPLE for `foreign_key`, inclusive integer bounds, exact integer aggregate equality, finite allowed tuples, and strict interval/predecessor ordering. Conformance vectors must provide the full `relations` and eight definition maps, using empty maps for definitions irrelevant to that vector.

- [ ] **Step 4: Run GREEN**

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_build data_integrity_catalog_uses_approved_profile_and_rule_order -- --nocapture`

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_contract`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/src/rules/families/data_integrity crates/spur-solver/tests/rule_manifest_build.rs
git commit -m "feat(spur-solver): manifest-bundle add data integrity catalog"
```

---

### Task 3: Build the strict snapshot model and compiler skeleton

**Task ID:** `typed-model`

**Files:**

- Create: `crates/spur-solver/src/rules/families/data_integrity.rs`
- Create: `crates/spur-solver/src/rules/families/data_integrity/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/mod.rs:1-14`

**Depends on:** `handler-abi`

**Acceptance Criteria:**

- [ ] Strict Serde types represent relations, integer/enum/Boolean field domains, rows, cells, eight definition maps, and the three explicit unknown kinds.
- [ ] Verification rejects unknown declarations; synthesis rejects null facts without matching unknown declarations.
- [ ] Duplicate/empty IDs, invalid domains, unknown references, incompatible composite keys, malformed tuples, inverted bounds, and incomplete fixed cells fail before lowering.
- [ ] Variables and projections are deterministic and caller-ordered; only declared unknowns produce assignment projections.
- [ ] Checked AST estimation rejects overflow and estimates above 16,384 before constraint construction.
- [ ] `data_integrity` is compiled as a module but is not yet added to the executable compiler array.

**Suggested Worker:** claude-code — new typed module with validation and resolver invariants.

**Scope Boundary:**

- IN scope: request/fact DTOs, input schema, validation/indexing, typed resolver, projection paths, checked budget estimator, compiler orchestration, deliberate unsupported-handler error until Tasks 4-5.
- OUT of scope: semantic lowering bodies, YAML changes, executable compiler-array registration, integration catalog snapshots.
- If the typed request requires a cross-crate type change, emit `scope_drift`; the design must fit existing `Variable`, `ConstraintExpr`, `CompiledRule`, and `ModelProjection` APIs.

**Implementation:**

- [ ] **Step 1: Write failing unit tests inside `compile.rs`**

Define local `empty_facts() -> Value`, `request(mode, rule_id, subject, facts) -> Value`, and `request_with_unknown(kind) -> Value` fixture builders using the exact public JSON shape. Cover strict unknown fields, verify-with-unknown rejection, missing declaration rejection, enum/domain mismatch, duplicate unknown target, composite-key type mismatch, and checked budget overflow. Include:

```rust
#[test]
fn verification_rejects_declared_snapshot_unknowns() {
    let error = compile(request_with_unknown("row_active"))
        .expect_err("verification facts must be complete");
    assert!(error.contains("verification requires complete data integrity facts"));
}

#[test]
fn expression_budget_uses_checked_arithmetic() {
    let error = estimate_expression_nodes(usize::MAX, 2, 2)
        .expect_err("overflow must fail before lowering");
    assert!(error.contains("expression budget overflow"));
}
```

- [ ] **Step 2: Run RED**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity -- --nocapture`

Expected: compilation fails because the module and types do not exist.

- [ ] **Step 3: Implement exact fact and unknown shapes**

```rust
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FieldDomainFacts {
    Integer { minimum: i64, maximum: i64 },
    Enum { values: Vec<String> },
    Boolean,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CellFacts {
    present: Option<bool>,
    value: Option<CellValueFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DataIntegrityUnknown {
    RowActive { relation: String, row: String },
    CellPresent { relation: String, row: String, field: String },
    CellValue { relation: String, row: String, field: String },
}
```

Represent semantic Booleans as solver integers constrained to `0..1`, so cardinality remains QF_LIA. Allocate typed value variables from field schemas; fixed facts add equality constraints, while only explicit unknown targets omit fixed equality and emit projections.

- [ ] **Step 4: Implement strict validation and budget estimates**

Use checked `add`/`mul` for unique `rows² × key_fields`, foreign key `child_rows × parent_rows × key_fields`, mutual consistency `rows × tuples × fields`, and temporal `rows + edges`. Reject totals above `MAX_CONSTRAINTS * MAX_VARIABLES`.

- [ ] **Step 5: Run GREEN**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity -- --nocapture`

Run: `scripts/spur-cargo check -p spur-solver`

- [ ] **Step 6: Commit**

```bash
git add crates/spur-solver/src/rules/families/data_integrity.rs crates/spur-solver/src/rules/families/data_integrity/compile.rs crates/spur-solver/src/rules/families/mod.rs
git commit -m "feat(spur-solver): typed-model add relational snapshot compiler"
```

---

### Task 4: Lower relational identity, reference, count, and range rules

**Task ID:** `relational-lowering`

**Files:**

- Modify: `crates/spur-solver/src/rules/families/data_integrity/compile.rs`

**Depends on:** `manifest-bundle`, `typed-model`

**Acceptance Criteria:**

- [ ] `unique` applies pairwise NULLS DISTINCT semantics over complete keys only.
- [ ] `foreign_key` applies MATCH SIMPLE semantics and finite parent-row disjunctions with exact arity/type checks.
- [ ] `cardinality` enforces inclusive active-row bounds over a relation or declared row subset.
- [ ] `value_range` applies inclusive bounds only to active rows with present integer values.
- [ ] Each binding produces one deterministic `CompiledRule` with exact rule/subject attribution.
- [ ] Valid, invalid, and bounded synthesis unit tests pass for all four handlers.

**Suggested Worker:** claude-code — solver lowering requires careful relational semantics and non-vacuity tests.

**Scope Boundary:**

- IN scope: four handler arms, reusable implication/key helpers, focused unit tests in `compile.rs`.
- OUT of scope: the remaining four handlers, manifest content, public compiler registration, frozen catalog fixture.
- If lowering needs quantifiers, arrays, strings, or nonlinear arithmetic, emit `risk`; all joins and pairwise comparisons must be finitely expanded.

**Implementation:**

- [ ] **Step 1: Add failing formula-shape and outcome tests**

In the existing `compile.rs` test module, define `unique_request(duplicate_complete_key: bool) -> Value` and `foreign_key_request(child_key_present: bool, parent_match: bool) -> Value`. Reuse an async `status(input: Value) -> SolveStatus` helper that compiles the family request and executes it with `SolverService::solve_constraints`.

```rust
#[tokio::test]
async fn unique_ignores_incomplete_keys_but_rejects_equal_complete_keys() {
    assert_eq!(status(unique_request(false)).await, SolveStatus::Sat);
    assert_eq!(status(unique_request(true)).await, SolveStatus::Unsat);
}

#[tokio::test]
async fn foreign_key_match_simple_accepts_absent_child_key() {
    assert_eq!(
        status(foreign_key_request(false, false)).await,
        SolveStatus::Sat
    );
}
```

Also assert the generated constraint names contain the binding index and definition subject.

- [ ] **Step 2: Run RED**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity::compile::tests::unique_ -- --nocapture`

Expected: the compiler reports an unsupported data-integrity handler.

- [ ] **Step 3: Implement finite QF_LIA lowerings**

Use `not(antecedent) or consequent` for implications. Expand unique pairs in caller row order, expand foreign-key parent candidates in caller order, sum active `0/1` expressions for cardinality, and reject `value_range` definitions targeting non-integer fields.

- [ ] **Step 4: Run GREEN**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity -- --nocapture`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/src/rules/families/data_integrity/compile.rs
git commit -m "feat(spur-solver): relational-lowering compile core integrity rules"
```

---

### Task 5: Lower conditional, aggregate, consistency, and temporal rules

**Task ID:** `advanced-lowering`

**Files:**

- Modify: `crates/spur-solver/src/rules/families/data_integrity/compile.rs`

**Depends on:** `relational-lowering`

**Acceptance Criteria:**

- [ ] `conditional_required` compares a schema-typed expected value and requires the target field only when its complete trigger holds.
- [ ] `aggregate_balance` requires every explicitly listed integer term cell to be present/resolvable and enforces the exact checked linear total; row activity is not an implicit filter over the explicit term list.
- [ ] `mutually_consistent` requires all participating fields and admits only one of the finite allowed typed tuples.
- [ ] `temporal_consistency` requires active intervals to have present integer endpoints with `start < end`, and enforces every declared `before -> after` edge as `after_active => before_active && end_before <= start_after`.
- [ ] Valid, invalid, synthesis, and counterexample tests pass for all four handlers.

**Suggested Worker:** claude-code — coupled typed comparison and temporal/aggregate solver work.

**Scope Boundary:**

- IN scope: remaining handler arms, typed-value equality helper, checked linear coefficient handling, focused unit tests.
- OUT of scope: decimal tolerance, relation-wide implicit aggregation, overlap algebra beyond strict intervals/predecessors, compiler registration, MCP response changes.
- Emit `risk` before changing the approved exact-integer or finite-tuple semantics.

**Implementation:**

- [ ] **Step 1: Add failing rule tests**

Define local `balance_request(left: i64, delta: i64, target: i64) -> Value` and `temporal_request(start: i64, end: i64, predecessor_end: i64, successor_start: i64) -> Value` builders, and reuse the Task 4 async `status` helper.

```rust
#[tokio::test]
async fn aggregate_balance_rejects_an_off_by_one_total() {
    assert_eq!(status(balance_request(100, -30, 70)).await, SolveStatus::Sat);
    assert_eq!(status(balance_request(100, -30, 69)).await, SolveStatus::Unsat);
}

#[tokio::test]
async fn temporal_consistency_rejects_reversed_interval_and_predecessor() {
    assert_eq!(
        status(temporal_request(5, 4, 6, 5)).await,
        SolveStatus::Unsat
    );
}
```

Add enum and Boolean conditional/tuple cases so typed equality is not tested only with integers.

- [ ] **Step 2: Run RED**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity::compile::tests::aggregate_ -- --nocapture`

Expected: the four handlers remain unsupported.

- [ ] **Step 3: Implement exact typed lowerings**

Build aggregate expressions with checked `i64` coefficients and existing linear `mul`/`add` primitives. Construct mutual-consistency as a finite OR of typed tuple conjunctions. Reuse active/present guards and generate separate named interval and predecessor constraints under the owning binding.

- [ ] **Step 4: Run GREEN**

Run: `scripts/spur-cargo test -p spur-solver --lib data_integrity -- --nocapture`

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_conformance`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/src/rules/families/data_integrity/compile.rs
git commit -m "feat(spur-solver): advanced-lowering compile complete integrity family"
```

---

### Task 6: Register the family and refresh stable catalog contracts

**Task ID:** `catalog-registration`

**Files:**

- Modify: `crates/spur-solver/src/rules/families/mod.rs:5-18`
- Modify: `crates/spur-solver/tests/platform_rule_catalog.rs:9-53`
- Modify: `crates/spur-solver/tests/rule_manifest_loader.rs:40-66`
- Modify: `crates/spur-solver/tests/solve_rules_mcp.rs:160-190`
- Modify: `crates/spur-solver/tests/rule_manifest_equivalence.rs:30-55`
- Modify: `crates/spur-solver/tests/fixtures/builtin_rule_catalog_v1.json`

**Depends on:** `advanced-lowering`, `manifest-bundle`

**Acceptance Criteria:**

- [ ] `families::compilers()` exposes eight compilers in stable family-ID order with `data_integrity` between `configuration` and `design`.
- [ ] Public family/rule schemas expose eight families and 39 executable rules.
- [ ] The frozen catalog contains 40 total rules, including the existing catalog-only RBAC rule.
- [ ] Exact stable ID arrays include all eight data-integrity rules in lexical order.
- [ ] Catalog, loader, equivalence, and MCP schema tests pass.

**Suggested Worker:** claude-code — coordinated multi-file stable-contract update.

**Scope Boundary:**

- IN scope: compiler-array registration, exact expected counts/IDs, canonical fixture refresh.
- OUT of scope: compiler semantics, manifest formulas, new response fields, unrelated catalog metadata.
- If serialized changes include anything beyond the new family/profile/rules, stop and emit `risk` before accepting the fixture diff.

**Implementation:**

- [ ] **Step 1: Register the compiler and run RED**

Change the array to:

```rust
static COMPILERS: [&dyn RuleFamilyCompiler; 8] = [
    &accessibility::COMPILER,
    &configuration::COMPILER,
    &data_integrity::COMPILER,
    &design::COMPILER,
    &policy::COMPILER,
    &resource::COMPILER,
    &scheduling::COMPILER,
    &workflow::COMPILER,
];
```

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_catalog -- --nocapture`

Expected RED: expected arrays/counts and frozen fixture still describe seven families and 31 executable rules.

- [ ] **Step 2: Update exact stable contracts**

Set family count to 8, executable count to 39, and frozen total rule count to 40. Insert the eight lexical IDs after configuration rules and before layout rules.

- [ ] **Step 3: Refresh and audit the canonical fixture**

Temporarily add an ignored test that prints `serde_json::to_string_pretty(builtin_registry())`, run it with:

`scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence print_builtin_catalog_fixture -- --ignored --nocapture`

Copy only the printed JSON into `builtin_rule_catalog_v1.json`, remove the temporary test, and inspect the semantic diff for exactly one family, one profile, and eight rules.

- [ ] **Step 4: Run GREEN**

Run: `scripts/spur-cargo test -p spur-solver --test platform_rule_catalog`

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_loader`

Run: `scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence`

Run: `scripts/spur-cargo test -p spur-solver --test solve_rules_mcp`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-solver/src/rules/families/mod.rs crates/spur-solver/tests/platform_rule_catalog.rs crates/spur-solver/tests/rule_manifest_loader.rs crates/spur-solver/tests/solve_rules_mcp.rs crates/spur-solver/tests/rule_manifest_equivalence.rs crates/spur-solver/tests/fixtures/builtin_rule_catalog_v1.json
git commit -m "feat(spur-solver): catalog-registration publish data integrity family"
```

---

### Task 7: Prove double evaluation, synthesis, attribution, and budgets end to end

**Task ID:** `execution-evidence`

**Files:**

- Create: `crates/spur-solver/tests/data_integrity_rule_execution.rs`

**Depends on:** `catalog-registration`

**Acceptance Criteria:**

- [ ] All eight manifest valid vectors return `sat` with `pass`; all eight invalid vectors return `unsat` with exact rule/subject diagnostics.
- [ ] Synthesis covers `row_active`, `cell_present`, and typed `cell_value` unknowns, with assignments in caller order.
- [ ] Verification rejects unknowns and incomplete facts before solver execution.
- [ ] One composed all-eight snapshot is satisfiable; one conflicting composition reports attributable rule results/UNSAT core names.
- [ ] Budget tests cover the largest accepted estimate, the first rejected estimate, and checked arithmetic overflow.
- [ ] Full `spur-solver` tests and formatting pass.

**Suggested Worker:** claude-code — integration-test authoring and failure interpretation across manifest, compiler, MCP, and solver layers.

**Scope Boundary:**

- IN scope: one dedicated integration-test file and verification commands.
- OUT of scope: production behavior changes; any discovered defect must be reported through review rather than silently expanding this test task.
- If a test exposes a production defect, emit `risk` with the failing request and expected/actual status so the brain can re-plan a focused fix.

**Implementation:**

- [ ] **Step 1: Add failing end-to-end tests**

Define the integration helper with the real MCP surface:

```rust
async fn solve(request: &Value) -> Value {
    let response = registry()
        .call_json_tool(context(), "solve_rules", request.clone())
        .await;
    result_json(&response)
}

#[tokio::test]
async fn every_data_integrity_manifest_vector_is_double_evaluated() {
    for rule_id in manifest_family_executable_rule_ids("data_integrity").unwrap() {
        let vectors = manifest_conformance_vectors(rule_id).unwrap();
        assert_eq!(solve(&vectors.valid[0].request).await["status"], "sat");
        let invalid = solve(&vectors.invalid[0].request).await;
        assert_eq!(invalid["status"], "unsat");
        assert_eq!(invalid["rule_results"][0]["rule_id"], rule_id.as_str());
    }
}
```

Add explicit synthesis requests for the three unknown kinds, an all-eight composed request, strict validation failures, and budget boundary requests.

- [ ] **Step 2: Run the dedicated test**

Run: `scripts/spur-cargo test -p spur-solver --test data_integrity_rule_execution -- --nocapture`

Expected: GREEN if Tasks 1-6 fully satisfy the spec; otherwise preserve the exact failure as review evidence and signal the owning task.

- [ ] **Step 3: Run final crate verification**

Run: `scripts/spur-cargo fmt --check`

Run: `scripts/spur-cargo test -p spur-solver`

Run: `scripts/spur-cargo check -p spur-solver`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-solver/tests/data_integrity_rule_execution.rs
git commit -m "test(spur-solver): execution-evidence prove data integrity contracts"
```

---

## Dependency DAG

```text
handler-abi
├── manifest-bundle ───────────────┐
└── typed-model ───────────────────┤
                                   ▼
                         relational-lowering
                                   ▼
                           advanced-lowering
                                   ▼
                         catalog-registration
                                   ▼
                          execution-evidence
```

`manifest-bundle` and `typed-model` are the only parallel branch. All later tasks are deliberately ordered because they share `compile.rs` or consume the complete manifest/compiler contract.

## Plan Self-Review

- Spec coverage: all eight rule formulas, three unknown kinds, strict verification, checked budgeting, attribution, manifest discovery, catalog publication, and double evaluation map to explicit tasks.
- Placeholder scan: no deferred implementation markers or unspecified validation steps remain.
- Type consistency: manifest subjects name definition IDs; Rust maps use the same eight definition categories; all handlers have empty caller-parameter ABIs.
- DAG validation: acyclic; parallel manifest/model work converges before semantic lowering.
- Beads compatibility: every task has a unique ID, dependencies, acceptance criteria, worker routing, and an explicit scope boundary.
