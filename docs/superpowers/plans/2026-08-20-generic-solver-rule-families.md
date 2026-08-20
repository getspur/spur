# Generic Solver Rule Families Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-20-generic-solver-rule-families-design.ipynb`
**Formal @spec cells:** `GENERIC-FAMILY-ROUTING`, `CONFIGURATION-FINITE-COMPATIBILITY`, `SCHEDULING-FINITE-HORIZON-FEASIBILITY`, `WORKFLOW-APPROVAL-SAFETY`
**Design epic:** `bd-3he` (closed)

**Goal:** Add catalog-visible, typed configuration, scheduling, and workflow rule families with verification, synthesis, optimization, and bounded counterexample behavior.

**Architecture:** Extend the existing YAML-manifest/native-compiler architecture. Each family owns strict facts, rule bindings, bounded unknowns, typed lowering, and model projection; the shared execution layer continues to own solver status and attribution.

**Tech Stack:** Rust 2021, serde/serde_json, embedded YAML manifests, typed B-prime constraints, Z3 through `SolverService`, `scripts/spur-cargo`.

---

### Task 1: Extend closed native handler contracts

**Task ID:** `generic-handlers`

**Files:**
- Modify: `crates/spur-solver/src/rules/manifest_format.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] All 14 new native handlers deserialize from their manifest names.
- [ ] Every handler reports exactly one owning family.
- [ ] Parameter ABI matches the approved spec, including optional makespan and reachability bounds.
- [ ] Existing manifest-format tests remain green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `NativeHandlerV1`, `ALL`, family ownership, parameter ABI, inline tests.
- OUT of scope: manifests, family compilers, solver execution, unrelated handlers.
- If another file is required, emit `scope_drift` before editing it.

**Implementation:**
- [ ] **Step 1: Write the failing test** that deserializes every new snake-case handler and asserts the expected family/ABI.

```rust
#[test]
fn generic_rule_handlers_have_closed_family_and_parameter_abis() {
    let handler: NativeHandlerV1 = serde_yaml::from_str("configuration_requires_any")
        .expect("new handler must deserialize");
    assert_eq!(handler.family(), "configuration");
    assert!(handler.parameter_abi().is_empty());
}
```

- [ ] **Step 2: Verify RED** with `scripts/spur-cargo test -p spur-solver generic_rule_handlers_have_closed_family_and_parameter_abis -- --nocapture`; expect deserialization/assertion failure because the variants are absent.
- [ ] **Step 3: Add the 14 enum variants**, exhaustive `ALL` entries, family arms, and parameter ABI arms.
- [ ] **Step 4: Verify GREEN** with the focused test and `scripts/spur-cargo test -p spur-solver manifest_format -- --nocapture`.
- [ ] **Step 5: Commit** as `feat(spur-solver): generic-handlers add generic rule handler contracts`.

### Task 2: Add configuration catalog manifests

**Task ID:** `configuration-catalog`

**Files:**
- Create: `crates/spur-solver/src/rules/families/configuration.rs`
- Create: `crates/spur-solver/src/rules/families/configuration/family.yaml`
- Create: `crates/spur-solver/src/rules/families/configuration/rules/requires_any.yaml`
- Create: `crates/spur-solver/src/rules/families/configuration/rules/excludes.yaml`
- Create: `crates/spur-solver/src/rules/families/configuration/rules/selection_cardinality.yaml`
- Create: `crates/spur-solver/src/rules/families/configuration/rules/attribute_allowed_pair.yaml`
- Create: `crates/spur-solver/src/rules/families/configuration/rules/version_interval.yaml`

**Depends on:** `generic-handlers`

**Acceptance Criteria:**
- [ ] Family/profile IDs are `configuration` / `finite_compatibility`.
- [ ] Five implemented hard rules contain authorities, valid/invalid examples, diagnostics, LLM encoding, solver encoding, and executable conformance vectors.
- [ ] Subject and parameter contracts match the design predicates.
- [ ] The embedded bundle validates without raw SMT in manifests.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: configuration entry module and configuration YAML files only.
- OUT of scope: compiler lowering, shared registration, other families.

**Implementation:**
- [ ] **Step 1: Add strict manifests** following the current resource/policy shape, with formula text and dual conformance vectors for all five rules.
- [ ] **Step 2: Run bundle validation** with `scripts/spur-cargo test -p spur-solver manifest -- --nocapture`; malformed handler names, ABIs, or examples must fail before correction.
- [ ] **Step 3: Correct only manifest-contract failures** until the bundle tests pass.
- [ ] **Step 4: Commit** as `feat(spur-solver): configuration-catalog add compatibility rule manifests`.

### Task 3: Implement configuration lowering

**Task ID:** `configuration-compiler`

**Files:**
- Create: `crates/spur-solver/src/rules/families/configuration/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/configuration.rs`
- Modify: `crates/spur-solver/src/rules/families/mod.rs`

**Depends on:** `configuration-catalog`

**Acceptance Criteria:**
- [ ] Strict facts support components, selection groups, explicit allowed attribute pairs, and ranked versions.
- [ ] Verification rejects incomplete facts; synthesis permits only declared bounded unknowns.
- [ ] All five handlers lower to typed Bool/Int/Enum constraints with stable IDs.
- [ ] Satisfiable projections return only caller-declared unknowns.
- [ ] Valid, invalid, synthesis, duplicate/reference, and bound tests pass.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: configuration compiler, module export, one registration entry.
- OUT of scope: scheduling/workflow files and shared execution semantics.

**Implementation:**
- [ ] **Step 1: Write RED compiler tests** for requires-any synthesis, pair exclusion failure, cardinality, allowed-pair rejection, and version-range synthesis.

```rust
#[test]
fn requires_any_synthesizes_a_selected_provider() {
    let compiled = compile(configuration_request_with_unknown_providers())
        .expect("configuration request must compile");
    assert_eq!(compiled.rules.len(), 1);
    assert_eq!(compiled.projections.len(), 2);
}
```

- [ ] **Step 2: Verify RED** with `scripts/spur-cargo test -p spur-solver configuration -- --nocapture`; expect missing compiler behavior.
- [ ] **Step 3: Implement strict request/fact parsing, validation, resolver, lowering, and projection** using only typed primitives.
- [ ] **Step 4: Verify GREEN** with the focused configuration tests and manifest conformance vectors.
- [ ] **Step 5: Re-run the approved configuration solve predicates** and record post-implementation solve IDs in the task audit.
- [ ] **Step 6: Commit** as `feat(spur-solver): configuration-compiler lower finite compatibility rules`.

### Task 4: Add scheduling catalog manifests

**Task ID:** `scheduling-catalog`

**Files:**
- Create: `crates/spur-solver/src/rules/families/scheduling.rs`
- Create: `crates/spur-solver/src/rules/families/scheduling/family.yaml`
- Create: `crates/spur-solver/src/rules/families/scheduling/rules/assignment_exactly_once.yaml`
- Create: `crates/spur-solver/src/rules/families/scheduling/rules/placement_allowed.yaml`
- Create: `crates/spur-solver/src/rules/families/scheduling/rules/precedence_finish_start.yaml`
- Create: `crates/spur-solver/src/rules/families/scheduling/rules/cumulative_capacity.yaml`
- Create: `crates/spur-solver/src/rules/families/scheduling/rules/minimize_makespan.yaml`

**Depends on:** `generic-handlers`

**Acceptance Criteria:**
- [ ] Family/profile IDs are `scheduling` / `finite_horizon`.
- [ ] Five implemented hard rules document discrete, non-preemptive, finite-horizon semantics.
- [ ] Makespan manifests distinguish objective synthesis from hard-bound verification.
- [ ] Authorities and dual conformance vectors cover feasible, infeasible, and optimization cases.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: scheduling entry module and scheduling YAML files only.
- OUT of scope: time-indexed lowering, shared registration, other families.

**Implementation:**
- [ ] **Step 1: Add strict manifests** with exact subject/parameter ABIs and checked examples.
- [ ] **Step 2: Run** `scripts/spur-cargo test -p spur-solver manifest -- --nocapture`; observe and fix only scheduling manifest-contract failures.
- [ ] **Step 3: Commit** as `feat(spur-solver): scheduling-catalog add allocation rule manifests`.

### Task 5: Implement scheduling lowering and optimization

**Task ID:** `scheduling-compiler`

**Files:**
- Create: `crates/spur-solver/src/rules/families/scheduling/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/scheduling.rs`
- Modify: `crates/spur-solver/src/rules/families/mod.rs`

**Depends on:** `scheduling-catalog`, `configuration-compiler`

**Acceptance Criteria:**
- [ ] Compiler builds bounded time-indexed 0/1 placements with checked horizon/model-size arithmetic.
- [ ] Exactly-once, eligibility/window, precedence, and cumulative-capacity constraints match the notebook formulas.
- [ ] Synthesis attaches typed `minimize Cmax`; verification requires and enforces `maximum_makespan`.
- [ ] Projection decodes machine/start assignments without exposing internal one-hot variables.
- [ ] Complete optimum and infeasible lower-bound tests pass.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: scheduling compiler, module export, one registration entry.
- OUT of scope: generic solver optimization semantics, configuration/workflow internals.

**Implementation:**
- [ ] **Step 1: Write RED tests** for two-machine feasibility, precedence conflict, unit-capacity non-overlap, decoded assignment projection, optimum `Cmax=4`, incomplete-bound rejection, and `Cmax≤3` infeasibility.

```rust
#[tokio::test]
async fn minimizes_the_two_machine_example_to_four_ticks() {
    let response = solve_scheduling(two_machine_three_job_request()).await;
    assert_eq!(response.solver.optimization.unwrap().termination, OptimizationTermination::Complete);
    assert_eq!(objective_value(&response), 4);
}
```

- [ ] **Step 2: Verify RED** with `scripts/spur-cargo test -p spur-solver scheduling -- --nocapture`.
- [ ] **Step 3: Implement normalized placements, constraints, objective emission, guards, and custom model decoding**.
- [ ] **Step 4: Verify GREEN** with focused scheduling tests.
- [ ] **Step 5: Post-solve the shipped formulation**; record the complete optimum and impossible-lower-bound solve IDs.
- [ ] **Step 6: Commit** as `feat(spur-solver): scheduling-compiler add bounded allocation optimization`.

### Task 6: Add workflow catalog manifests

**Task ID:** `workflow-catalog`

**Files:**
- Create: `crates/spur-solver/src/rules/families/workflow.rs`
- Create: `crates/spur-solver/src/rules/families/workflow/family.yaml`
- Create: `crates/spur-solver/src/rules/families/workflow/rules/initial_state_allowed.yaml`
- Create: `crates/spur-solver/src/rules/families/workflow/rules/transition_allowed.yaml`
- Create: `crates/spur-solver/src/rules/families/workflow/rules/safety_invariant.yaml`
- Create: `crates/spur-solver/src/rules/families/workflow/rules/bounded_reachability.yaml`

**Depends on:** `generic-handlers`

**Acceptance Criteria:**
- [ ] Family/profile IDs are `workflow` / `bounded_trace`.
- [ ] Four rules state finite-domain, finite-horizon semantics and bound provenance.
- [ ] Examples distinguish observed-trace verification from unsafe-target counterexample synthesis.
- [ ] Non-goals explicitly exclude unbounded liveness and fairness.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: workflow entry module and workflow YAML files only.
- OUT of scope: trace lowering, shared registration, other families.

**Implementation:**
- [ ] **Step 1: Add strict manifests** with official TLA+/SCXML/Apalache authorities and dual conformance vectors.
- [ ] **Step 2: Run** `scripts/spur-cargo test -p spur-solver manifest -- --nocapture`; observe and fix only workflow manifest-contract failures.
- [ ] **Step 3: Commit** as `feat(spur-solver): workflow-catalog add bounded trace rule manifests`.

### Task 7: Implement workflow lowering

**Task ID:** `workflow-compiler`

**Files:**
- Create: `crates/spur-solver/src/rules/families/workflow/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/workflow.rs`
- Modify: `crates/spur-solver/src/rules/families/mod.rs`

**Depends on:** `workflow-catalog`, `scheduling-compiler`

**Acceptance Criteria:**
- [ ] Strict facts support finite state/event enums, initial/safe/target sets, transition relations, and bounded trace slots.
- [ ] Verification rejects incomplete traces; synthesis creates only declared state/event slots.
- [ ] Initial, transition, safety, and bounded reachability predicates match the notebook formulas.
- [ ] Projection returns trace-indexed state/event assignments.
- [ ] Valid path, illegal transition, reachable unsafe witness, and unreachable target tests pass.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: workflow compiler, module export, one registration entry.
- OUT of scope: unbounded model checking, scheduling/configuration internals, execution status mapping.

**Implementation:**
- [ ] **Step 1: Write RED tests** for initial-state rejection, earliest illegal edge, approval safety, unsafe-target witness, and bounded UNSAT proof.

```rust
#[test]
fn unsafe_target_query_builds_a_bounded_counterexample_predicate() {
    let compiled = compile(approval_trace_with_unknown_slots()).expect("compile");
    assert_eq!(compiled.projections.len(), 5);
    assert_eq!(compiled.rules.last().unwrap().rule_id, "workflow.bounded_reachability");
}
```

- [ ] **Step 2: Verify RED** with `scripts/spur-cargo test -p spur-solver workflow -- --nocapture`.
- [ ] **Step 3: Implement finite enum variables, relation disjunctions, invariant/reachability lowering, validation, and projection**.
- [ ] **Step 4: Verify GREEN** with focused workflow tests.
- [ ] **Step 5: Post-solve the shipped bounded counterexample and no-counterexample queries**; record solve IDs and bounds.
- [ ] **Step 6: Commit** as `feat(spur-solver): workflow-compiler add bounded transition verification`.

### Task 8: Add cross-family catalog and execution coverage

**Task ID:** `generic-family-integration`

**Files:**
- Modify: `crates/spur-solver/src/mcp.rs`
- Modify: `crates/spur-solver/src/rules/spec.rs`
- Modify: `crates/spur-solver/src/rules/execute.rs`

**Depends on:** `configuration-compiler`, `scheduling-compiler`, `workflow-compiler`

**Acceptance Criteria:**
- [ ] `solve_rule_spec` lists seven families and all 31 executable rule IDs in stable order.
- [ ] Generated `solve_rules` schema contains the three new family branches and exact rule-ID enums.
- [ ] End-to-end prepare/run tests cover verification, synthesis, optimization, and bounded counterexample outcomes.
- [ ] Existing four families remain wire-compatible and green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: cross-family schema, discovery, and execution tests in the listed files.
- OUT of scope: changing family formulas, solver backend semantics, unrelated MCP tools.

**Implementation:**
- [ ] **Step 1: Write RED integration tests** expecting the new family cards, schema branches, stable compiler ordering, and representative end-to-end results.
- [ ] **Step 2: Verify RED** with `scripts/spur-cargo test -p spur-solver generic_family -- --nocapture`.
- [ ] **Step 3: Make only integration-layer adjustments required by the tests**; dynamic manifest/schema derivation should handle ordinary additions.
- [ ] **Step 4: Verify GREEN** with focused integration tests and `scripts/spur-cargo test -p spur-solver`.
- [ ] **Step 5: Run** `scripts/spur-cargo fmt --check` and `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-solver -- -D warnings`.
- [ ] **Step 6: Commit** as `test(spur-solver): generic-family-integration cover new rule families`.

## Dependency DAG

```text
generic-handlers
├── configuration-catalog → configuration-compiler
├── scheduling-catalog ───────────────────────┐
└── workflow-catalog ─────────────────────────┼──────────────────────────────┐
                                              │                              │
configuration-compiler + scheduling-catalog → scheduling-compiler            │
scheduling-compiler + workflow-catalog ─────→ workflow-compiler               │
configuration-compiler + scheduling-compiler + workflow-compiler ────────────→ generic-family-integration
```

Shared `families/mod.rs` edits are serialized by the additional compiler dependencies: scheduling follows configuration, and workflow follows scheduling. Catalog manifest tasks remain parallel because their file scopes do not overlap.
