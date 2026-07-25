# Z3 Constraint Solver Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan`.  
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md` (HEAD includes S0.a/S0.b review amendments)  
**Preferred worker:** `@codex` profile `rust-engineer` model `gpt-5.6-sol` effort `max`  
**Base:** `main`

**Goal:** Ship `crates/spur-solver` + MCP tools (`solve_constraints`, `solve_smt`, `get_solve_result`) for brain and worker agents using subprocess Z3 and B′ typed constraints.

**Architecture:** New crate owns types, encoder, `SolverService` (process-wide semaphore + Z3 spawn), persist under `.spur/solver/`. Thin `SolverMcpModule` wired into brain/worker/catalog registries in `spur-core`. No `z3`/`z3-sys` link.

**Tech Stack:** Rust 2021 workspace, tokio process, serde/serde_json, spur-mcp ToolModule, scripts/spur-cargo for build/test.

---

### Task 1: Scaffold `spur-solver` crate

**Task ID:** `solver-scaffold`

**Files:**
- Create: `crates/spur-solver/Cargo.toml`
- Create: `crates/spur-solver/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` + path dep alias if used elsewhere)
- Modify: any workspace dependency list needed (`serde`, `serde_json`, `tokio`, `thiserror`, `async-trait`, `uuid` or rand for ids)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `scripts/spur-cargo check -p spur-solver` succeeds
- [ ] Crate is workspace member; no `z3`/`z3-sys` dependency

**Suggested Worker:** codex / rust-engineer

**Scope Boundary:**
- IN: crate skeleton, workspace membership, empty module tree stubs (`types`, `encode`, `process`, `service`, `persist`, `smt_gate`, `mcp`)
- OUT: real solver logic, registry wiring

**Implementation notes:** Follow `spur-graph` package metadata style (`version.workspace`, `edition.workspace`, `license.workspace`, `dist = false`).

---

### Task 2: B′ types + ConstraintExpr validation

**Task ID:** `solver-types`

**Files:**
- Create/modify: `crates/spur-solver/src/types.rs`
- Create/modify: `crates/spur-solver/src/validate.rs`
- Create: unit tests in `types.rs` / `validate.rs` or `tests/validate.rs`
- Modify: `lib.rs` exports

**Depends on:** `solver-scaffold`

**Acceptance Criteria:**
- [ ] Tagged `ConstraintExpr` wire form per spec (no bare string leaves)
- [ ] Enum arithmetic rejected; name regex enforced; caps enforced
- [ ] Unit tests cover happy path + injection-ish names + enum misuse
- [ ] `scripts/spur-cargo test -p spur-solver` passes (no Z3 required)

**Suggested Worker:** codex / rust-engineer

**Scope Boundary:**
- IN: request/response types, validation only
- OUT: SMT generation, process spawn

---

### Task 3: B′ → SMT-LIB2 encoder + mangling

**Task ID:** `solver-encode`

**Files:**
- Create/modify: `crates/spur-solver/src/encode.rs`
- Tests asserting generated SMT substrings / structure

**Depends on:** `solver-types`

**Acceptance Criteria:**
- [ ] Declares vars, asserts bounds, encodes ops, ends with check-sat + get-value list
- [ ] Identifier mangling + surface names in models documented in code
- [ ] No agent string concat into SMT
- [ ] Unit tests without Z3

**Suggested Worker:** codex / rust-engineer

---

### Task 4: Z3Process + SolverService (fake runner)

**Task ID:** `solver-service`

**Files:**
- Create: `crates/spur-solver/src/process.rs`
- Create: `crates/spur-solver/src/service.rs`
- Create: test fake solver script under `crates/spur-solver/tests/fixtures/` or inline shell
- Tests: sat/unsat/timeout/kill/semaphore budget

**Depends on:** `solver-encode`

**Acceptance Criteria:**
- [ ] `SolverService` discovers Z3 via `SPUR_Z3_BIN` then PATH; version probe optional at boot
- [ ] Process-wide semaphore default 4; wait consumes timeout budget
- [ ] Wall-clock timeout kill (Unix process group; Windows best-effort per spur-acp patterns)
- [ ] Abstract runner trait; tests use fake binary/script — **no real Z3 required**
- [ ] Status taxonomy: sat|unsat|unknown|timeout|error never collapses unknown→unsat
- [ ] `scripts/spur-cargo test -p spur-solver` green

**Suggested Worker:** codex / rust-engineer

---

### Task 5: Persist artifacts + get_solve_result API

**Task ID:** `solver-persist`

**Files:**
- Create: `crates/spur-solver/src/persist.rs`
- Modify: `service.rs` for `persist: true`
- Modify: `.gitignore` to ignore `.spur/solver/`
- Tests: atomic write, regex solve_id, path traversal reject, quota

**Depends on:** `solver-service`

**Acceptance Criteria:**
- [ ] `solve_id` = `^sol_[0-9a-f]{16}$` validated pre-path-join
- [ ] Artifact schema v1 JSON under `<repo_root>/.spur/solver/<id>.json`
- [ ] Atomic write; load via service API used by MCP later
- [ ] `.gitignore` covers `.spur/solver/`
- [ ] Unit tests pass without Z3

**Suggested Worker:** codex / rust-engineer

---

### Task 6: Raw SMT allowlist gate + solve path

**Task ID:** `solver-smt-gate`

**Files:**
- Create: `crates/spur-solver/src/smt_gate.rs`
- Wire into `SolverService::solve_smt`
- Tests: allowlisted script ok; disallowed command rejected whole

**Depends on:** `solver-service`

**Acceptance Criteria:**
- [ ] Allowlist per design spec; reject-only (no silent strip)
- [ ] Size cap 256 KiB
- [ ] Unit tests without Z3

**Suggested Worker:** codex / rust-engineer

---

### Task 7: SolverMcpModule + registry wiring

**Task ID:** `solver-mcp`

**Files:**
- Create: `crates/spur-solver/src/mcp.rs` (`SolverMcpModule`, tool defs)
- Modify: `crates/spur-core/src/mcp/mod.rs` — register live module in `brain_tool_registry_with_local_projects`, `worker_tool_registry_with_client`, and catalog listing path
- Modify: `crates/spur-core/Cargo.toml` — depend on `spur-solver`
- Tests: registry lists `solve_constraints`, `solve_smt`, `get_solve_result`; not in `WORKER_DENIED_TOOL_CALLS`

**Depends on:** `solver-persist`, `solver-smt-gate`

**Acceptance Criteria:**
- [ ] Tools callable on brain + worker registries (or catalog_only list shape for catalog)
- [ ] Shared `SolverService` injection (Arc) — not per-call re-discover storms without sharing
- [ ] `scripts/spur-cargo test -p spur-core` / targeted MCP registry tests pass
- [ ] `scripts/spur-cargo check -p spur-core -p spur-solver` clean

**Suggested Worker:** codex / rust-engineer

**Scope Drift Checkpoint:** If wiring requires large spur-cli changes beyond registry composition, emit `scope_drift`.

---

### Task 8: Env-gated real Z3 smoke + docs touch

**Task ID:** `solver-verify`

**Files:**
- Create: `crates/spur-solver/tests/z3_smoke.rs` (ignored unless `SPUR_TEST_Z3=1`)
- Modify: short note in design plan or `docs/superpowers/specs/...` status → Implemented-in-progress only if needed
- Ensure examples fixtures as unit tests for constraint satisfaction (not unique models)

**Depends on:** `solver-mcp`

**Acceptance Criteria:**
- [ ] Default CI/test without Z3 still green
- [ ] With `SPUR_TEST_Z3=1` and `z3` on PATH, smoke solve returns sat for trivial int problem
- [ ] `scripts/spur-cargo test -p spur-solver` and registry tests green without Z3

**Suggested Worker:** codex / rust-engineer

---

## DAG

```
solver-scaffold
    → solver-types
        → solver-encode
            → solver-service
                → solver-persist ──┐
                → solver-smt-gate ─┴→ solver-mcp → solver-verify
```

## Spec coverage map

| Spec area | Task |
|---|---|
| Crate + no FFI | scaffold, service |
| B′ + ConstraintExpr | types, encode |
| SolverService / semaphore / kill | service |
| Persist / solve_id / gitignore | persist |
| solve_smt allowlist | smt-gate |
| MCP + registries | mcp |
| Status taxonomy / tests | service, verify |
