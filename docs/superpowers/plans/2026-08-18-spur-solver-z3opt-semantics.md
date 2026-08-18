# Spur Solver Z3 Optimize Semantics Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-18-spur-solver-z3opt-semantics-design.ipynb`
**Formal @spec cells:** `Z3OPT-PROTOCOL`, `OBJECTIVE-BOUND-CLASSIFIER`
**Design epic:** `bd-b37` (closed)

**Goal:** Make `spur-solver` preserve aggregate weighted-MaxSMT semantics, expose exact optimization results, enumerate bounded Pareto/box solutions, and reject unsupported production Z3 versions.

**Architecture:** Preserve the current external `z3 -in` boundary. Extend the typed request/response contract first, then teach the encoder to emit complete optimization cycles and the parser to consume them positionally. Keep backward-compatible top-level status/model fields while placing new multi-solution data in an additive optimization envelope.

**Tech Stack:** Rust 2021, serde, tokio, SMT-LIB2, Z3 Optimize/νZ, `scripts/spur-cargo`.

---

### Task 1: Capture optimization regressions as RED tests

**Task ID:** `z3opt-tests`

**Files:**
- Create: `crates/spur-solver/tests/optimization_protocol.rs`
- Modify: `crates/spur-solver/tests/real_z3.rs`
- Modify: `crates/spur-solver/tests/smt_gate.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Current code fails the targeted encoder assertions for diagnostic IDs, soft-only priority, and `get-objectives`.
- [ ] Current code fails the real-Z3 weighted-soft, bound, Pareto, and box assertions for the audited reasons.
- [ ] Current raw gate rejects the new `get-objectives` acceptance test.
- [ ] Only tests are changed; no production code is modified.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three listed test files and reusable test-only helpers inside them.
- OUT of scope: all `src/` files, README, workspace manifests, and unrelated solver rules.
- If a public seam is insufficient for a test, emit `scope_drift`; do not add test-only production APIs.

**Implementation:**
- [ ] **Step 1: Add an encoder regression using JSON so the desired grouping contract compiles before the new Rust field exists.**

```rust
#[test]
fn diagnostic_soft_ids_do_not_create_z3_objective_groups() {
    let request: SolveConstraintsRequest = serde_json::from_value(json!({
        "vars": [{"type":"bool","name":"a"}, {"type":"bool","name":"b"}],
        "constraints": [
            {"id":"prefer_a","soft":true,"weight":1,
             "expr":{"kind":"var","name":"a"}},
            {"id":"prefer_b","soft":true,"weight":100,
             "expr":{"kind":"var","name":"b"}}
        ]
    })).unwrap();
    let smt = encode_solve_constraints(&request).unwrap();
    assert!(!smt.contains(":id prefer_a"));
    assert!(!smt.contains(":id prefer_b"));
    assert!(smt.contains("(get-objectives)"));
}
```

- [ ] **Step 2: Add soft-only priority and explicit shared-group JSON cases.**

```rust
assert!(pareto_soft_only_smt.contains("(set-option :opt.priority pareto)"));
assert_eq!(shared_group_smt.matches(":id preferences").count(), 2);
```

- [ ] **Step 3: Add ignored real-Z3 cases.** Use `¬(a ∧ b)` with weights 1 and 100 and unique diagnostic IDs; assert `a=false, b=true`. Add `maximize x` for unconstrained Real and `x < 1`; assert serialized bounds are `infinite` and `strict`. Add `x+y<=3` Pareto and two-objective box cases; compare returned model sets, not enumeration order.

- [ ] **Step 4: Add the raw gate assertion.**

```rust
assert!(validate_smt_script("(check-sat)\n(get-objectives)").is_ok());
```

- [ ] **Step 5: Run RED and record the exact failures.**

```bash
scripts/spur-cargo test -p spur-solver --test optimization_protocol
SPUR_REMOTE=0 SPUR_TEST_Z3=1 scripts/spur-cargo test -p spur-solver --test real_z3 -- --ignored
scripts/spur-cargo test -p spur-solver --test smt_gate
```

Expected: assertions fail because diagnostic IDs become objective groups, objective output is absent, only one Pareto/box model is returned, bounds are absent, and `get-objectives` is rejected.

- [ ] **Step 6: Commit the RED tests.**

```bash
git add crates/spur-solver/tests/optimization_protocol.rs crates/spur-solver/tests/real_z3.rs crates/spur-solver/tests/smt_gate.rs
git commit -m "test(spur-solver): z3opt-tests capture optimization regressions"
```

---

### Task 2: Extend the typed optimization contract

**Task ID:** `z3opt-types`

**Files:**
- Modify: `crates/spur-solver/src/types.rs`
- Modify: `crates/spur-solver/src/mcp.rs`

**Depends on:** `z3opt-tests`

**Acceptance Criteria:**
- [ ] `ConstraintDecl.id` is diagnostic-only and `group` is repeatable, identifier-validated, and soft-only.
- [ ] `max_solutions` defaults to 16 and validates in `1..=64`.
- [ ] Additive response types represent termination, exact bound classification, objective results, soft constraint results, group costs, and per-solution models.
- [ ] MCP JSON schema advertises the complete new request contract.
- [ ] Existing request JSON and non-optimization response JSON remain valid.

**Suggested Worker:** claude-code

**Scope Boundary:**
- IN scope: public types, validation, serde defaults/invariants, MCP input schema, and their existing inline unit tests.
- OUT of scope: SMT emission, stdout parsing, process execution, raw gate, and documentation.
- Emit `scope_drift` before touching another file.

**Implementation:**
- [ ] **Step 1: Run the relevant RED cases from Task 1 and existing validation/schema tests.**

```bash
scripts/spur-cargo test -p spur-solver --test optimization_protocol
scripts/spur-cargo test -p spur-solver mcp::tests::solver_tool_schemas_cover_the_full_request_contract
```

- [ ] **Step 2: Add bounded request constants and fields.**

```rust
pub const DEFAULT_MAX_SOLUTIONS: usize = 16;
pub const MAX_SOLUTIONS: usize = 64;

pub struct ConstraintDecl {
    pub id: Option<String>,
    pub group: Option<String>,
    pub soft: bool,
    pub weight: Option<i64>,
    pub expr: ConstraintExpr,
}
```

Reject `group` on hard constraints, but do not add groups to the unique-ID set.

- [ ] **Step 3: Add response types with lossless exact values.**

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObjectiveBound {
    Finite { exact: String },
    Infinite { exact: String },
    Strict { exact: String },
}

pub struct OptimizationResult {
    pub priority: Option<ObjectivePriority>,
    pub solutions: Vec<OptimizationSolution>,
    pub termination: OptimizationTermination,
}
```

`OptimizationSolution` owns a model, explicit objective results, per-soft satisfaction rows, and per-group weight totals. Keep top-level `model` as the first solution.

- [ ] **Step 4: Update response validation and MCP schema.** Ensure optimization is present only on satisfiable responses, contains at least one solution, and its first model equals the top-level model.

- [ ] **Step 5: Run targeted GREEN tests and formatting.**

```bash
scripts/spur-cargo test -p spur-solver types::tests
scripts/spur-cargo test -p spur-solver mcp::tests
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-solver/src/types.rs crates/spur-solver/src/mcp.rs
git commit -m "feat(spur-solver): z3opt-types add typed optimization results"
```

---

### Task 3: Encode faithful Optimize cycles

**Task ID:** `z3opt-encode`

**Files:**
- Modify: `crates/spur-solver/src/encode.rs`

**Depends on:** `z3opt-types`

**Acceptance Criteria:**
- [ ] Only `ConstraintDecl.group` is emitted as soft `:id`; anonymous soft constraints share Z3's aggregate default group.
- [ ] Soft-only optimization emits `:opt.priority`.
- [ ] Every sat cycle requests objectives before a combined values query.
- [ ] Lex emits one cycle, box emits one cycle per generated objective plus a terminal probe, and Pareto emits `max_solutions` cycles plus a terminal probe.
- [ ] Existing feasibility and unsat-core scripts retain their prior shape.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: encoder helpers and inline encoder tests only.
- OUT of scope: public type definitions, response parsing, process/version code, raw gate, and README.
- Emit `scope_drift` if parser changes appear necessary.

**Implementation:**
- [ ] **Step 1: Re-run encoder RED tests.**

```bash
scripts/spur-cargo test -p spur-solver --test optimization_protocol diagnostic_soft_ids_do_not_create_z3_objective_groups
```

- [ ] **Step 2: Introduce an optimization-active predicate and objective descriptor count.** Count unique soft groups by first declaration, treating all `group=None` entries as one group, then append explicit objectives.

- [ ] **Step 3: Change soft emission.**

```rust
if let Some(group) = constraint.group() {
    self.output.push(" :id ")?;
    self.output.push(group)?;
}
```

- [ ] **Step 4: Emit the cycle payload.**

```text
(check-sat)
(get-objectives)
(get-value (<declared vars> <soft expressions> <objective expressions>))
```

The terminal probe for Pareto/box is `check-sat` only, so the parser can distinguish complete enumeration from truncation without generating a final unavailable-model error.

- [ ] **Step 5: Run GREEN and regression tests.**

```bash
scripts/spur-cargo test -p spur-solver encode::tests
scripts/spur-cargo test -p spur-solver --test optimization_protocol
```

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-solver/src/encode.rs
git commit -m "fix(spur-solver): z3opt-encode preserve Optimize semantics"
```

---

### Task 4: Parse multi-solution results and enforce Z3 compatibility

**Task ID:** `z3opt-parse`

**Files:**
- Modify: `crates/spur-solver/src/service.rs`
- Modify: `crates/spur-solver/src/process.rs`

**Depends on:** `z3opt-encode`

**Acceptance Criteria:**
- [ ] Typed stdout parsing is positional and strict across lex, Pareto, and box cycles.
- [ ] Objective bound classification preserves exact S-expression text and matches the formal three-way partition.
- [ ] Soft satisfaction and group costs are derived from returned Boolean values.
- [ ] Pareto/box terminal statuses produce `complete`, `solution_limit`, or partial `unknown` semantics from the spec.
- [ ] Response includes the probed solver version; parseable production versions below 4.8.12 fail before optimization execution.
- [ ] Raw stdout containing `get-objectives` is tolerated and exposed in an optimization envelope with unknown typed metadata.

**Suggested Worker:** claude-code

**Scope Boundary:**
- IN scope: service response construction/parsing, S-expression rendering helpers, production version parsing/checking, and inline tests in these two files.
- OUT of scope: encoder, public type/schema definitions, raw command allowlist, real-Z3 test fixtures, and README.
- If the selected public types cannot represent a real Z3 form, emit `risk` before changing them.

**Implementation:**
- [ ] **Step 1: Run parser-facing RED tests with the Task 3 wire format.**

```bash
scripts/spur-cargo test -p spur-solver --test optimization_protocol
```

- [ ] **Step 2: Replace single-model `ParsedSolve::Sat` with a parsed optimization payload.** Consume each status, then one `(objectives ...)` form and one combined values form. Validate exact counts from the request.

- [ ] **Step 3: Classify bounds in the formal priority order.**

```rust
fn classify_bound(expr: &SExpression) -> ObjectiveBound {
    let exact = expr.to_smt2();
    if expr.contains_atom("oo") {
        ObjectiveBound::Infinite { exact }
    } else if expr.contains_atom("epsilon") {
        ObjectiveBound::Strict { exact }
    } else {
        ObjectiveBound::Finite { exact }
    }
}
```

- [ ] **Step 4: Build solution diagnostics.** Zip soft Boolean values with request constraint indices/IDs/groups/weights; aggregate satisfied and violated weights by group without changing their declaration order.

- [ ] **Step 5: Add version parsing and compatibility checks.** Accept unknown injected runner versions, parse `Z3 version M.m.p`, and reject parseable versions lower than `(4, 8, 12)` only for optimization-active requests.

- [ ] **Step 6: Run GREEN parser/service/process tests.**

```bash
scripts/spur-cargo test -p spur-solver service::tests
scripts/spur-cargo test -p spur-solver process::tests
scripts/spur-cargo test -p spur-solver --test optimization_protocol
```

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-solver/src/service.rs crates/spur-solver/src/process.rs
git commit -m "feat(spur-solver): z3opt-parse expose exact optimization results"
```

---

### Task 5: Allow raw objective retrieval

**Task ID:** `z3opt-raw-gate`

**Files:**
- Modify: `crates/spur-solver/src/smt_gate.rs`

**Depends on:** `z3opt-tests`

**Acceptance Criteria:**
- [ ] `get-objectives` is allowlisted as a read-only command.
- [ ] Existing forbidden commands/options remain rejected.
- [ ] Raw-gate regression tests from Task 1 pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: raw SMT command allowlist and its inline documentation.
- OUT of scope: raw output parsing, typed encoding, public types, tests owned by Task 1, and README.
- Emit `scope_drift` before touching another file.

**Implementation:**
- [ ] **Step 1: Run the RED gate test.**

```bash
scripts/spur-cargo test -p spur-solver --test smt_gate
```

- [ ] **Step 2: Add only `get-objectives` to `is_allowed_command` and its rustdoc list.**
- [ ] **Step 3: Run GREEN and the full gate suite.**

```bash
scripts/spur-cargo test -p spur-solver --test smt_gate
```

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-solver/src/smt_gate.rs
git commit -m "fix(spur-solver): z3opt-raw-gate allow objective retrieval"
```

---

### Task 6: Align documentation and run the real-Z3 matrix

**Task ID:** `z3opt-docs`

**Files:**
- Modify: `crates/spur-solver/README.md`

**Depends on:** `z3opt-parse`, `z3opt-raw-gate`

**Acceptance Criteria:**
- [ ] README explains diagnostic IDs versus soft groups, exact bounds, termination, and Pareto/box collection.
- [ ] The documented real-Z3 command includes `-- --ignored`.
- [ ] Deferred/non-goal claims no longer contradict implemented Real/BitVec/Pareto/box behavior.
- [ ] Full crate tests and all six-plus ignored real-Z3 tests pass on Z3 4.16.0.
- [ ] `cargo fmt` check is clean through `scripts/spur-cargo`.

**Suggested Worker:** claude-code

**Scope Boundary:**
- IN scope: README and verification commands.
- OUT of scope: production or test code. Signal `risk` if verification reveals a defect; do not patch it inside this task.

**Implementation:**
- [ ] **Step 1: Update request/response examples and the support matrix.** Include one anonymous aggregate soft example, one shared named group example, and a bounded Pareto response with termination.
- [ ] **Step 2: Correct the test command.**

```bash
SPUR_REMOTE=0 SPUR_TEST_Z3=1 scripts/spur-cargo test -p spur-solver --test real_z3 -- --ignored
```

- [ ] **Step 3: Run final verification.**

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-solver
SPUR_REMOTE=0 SPUR_TEST_Z3=1 scripts/spur-cargo test -p spur-solver --test real_z3 -- --ignored
```

- [ ] **Step 4: Commit documentation only.**

```bash
git add crates/spur-solver/README.md
git commit -m "docs(spur-solver): z3opt-docs document Optimize result semantics"
```

---

## Dependency DAG

```text
z3opt-tests
├── z3opt-types → z3opt-encode → z3opt-parse ─┐
└── z3opt-raw-gate ───────────────────────────┴→ z3opt-docs
```

## Plan self-review

- Spec coverage: all four audited gaps plus version compatibility and documentation are assigned.
- Placeholder scan: no TBD/TODO/fill-later steps.
- Type consistency: Task 2 defines every response/request type consumed by Tasks 3–4.
- DAG: acyclic; the independent raw-gate branch runs beside the typed API/encoder/parser chain.
- File isolation: no concurrently runnable tasks share planned write files.
