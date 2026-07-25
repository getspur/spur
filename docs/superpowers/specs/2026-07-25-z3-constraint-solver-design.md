# Z3 Constraint Solver for Coding Agents — Design

**Status:** Approved (brainstorming) — **post dual architect-reviewer amendments**  
**Date:** 2026-07-25  
**Crate (proposed):** `spur-solver`  
**Reviews:**
- claude-code `architect-reviewer` @ `7b7e5a20` → Accept-with-changes (P0/P1 folded)
- codex `architect-reviewer` @ `21688cae` → Accept-with-changes on pre-patch `f963b1a9d`; residual items folded below (Codex did not see the first amendment commit)

**Related prior art:** [alejandroqh/z39](https://github.com/alejandroqh/z39) (agent MCP over subprocess Z3)

## Purpose

Give SPUR brain and worker agents a **constraint model-finder** so coding work starts from **solved values** instead of invented parameters.

**Product thesis:** LLMs propose constraints; Z3 (or an equivalent SMT solver) returns `sat` + a concrete model (or `unsat` / `unknown` / `timeout`). Coding agents consume the model in config, layout, infra, tests, and constants.

**v1 job:** model-finding only. Proof/refutation, unsat cores, and optimization are deferred.

## Value

| Without resolver | With resolver |
|---|---|
| Agents invent buffer sizes, grid px, replica counts | One **checkable** feasible assignment |
| Silent rule violations ship | **unsat** surfaces impossibility before code |
| Brain→worker drift on “reasonable” numbers | Shared `solve_id` / model map |

## Locked decisions

| Decision | Choice |
|---|---|
| v1 capability | Find concrete values (`sat` + model) |
| Input | Hybrid: typed JSON (default) + raw SMT-LIB2 escape |
| Callers | Brain **and** workers (same tools) |
| JSON domains (B′) | `bool`, `int`, `int_range{min,max}`, `enum{values}` |
| Architecture | Approach 2: in-tree `spur-solver` + MCP module; **Z3 subprocess** (no FFI / no `z3` crate link) |
| Handoff | Persist optional `solve_id` under `.spur/solver/` for brain→worker |
| Z3 install (v1) | Discover via env/`PATH` only; clear error if missing |
| Timeout | **Default 30s**; **hard cap 60s** (never above cap) |
| Concurrency | Process-wide semaphore; default **4** concurrent Z3 children |
| Constraint language | **Closed** AST ops only; no string-concat into SMT from agent fragments |
| Optimization (νZ) | Out of v1; agents re-query with tighter bounds (“prefer X” doctrine) |

## Non-goals (v1)

- Proof generation, validity checking, unsat cores, interpolants
- νZ maximize/minimize as first-class API
- Theories in JSON: Real, BitVec, String, Array, Float, quantifiers (use `solve_smt` if needed)
- Incremental sessions (`push`/`pop`), process pooling as a product feature
- Solver portfolio (cvc5, Bitwuzla, …)
- Bundling Z3 into `cargo xtask dist` / auto-download (may revisit)
- TUI surface for solves
- Domain toys (calendar scheduling, dinner seating) as first-class tools
- Replacing typecheckers or full program verification

## Architecture

```
Agent (brain | worker)
  → MCP: solve_constraints | solve_smt | get_solve_result
    → SolverMcpModule (ToolModule; thin adapter)
      → shared SolverService (process-wide, injected)
           ├─ types + validate
           ├─ B′ encoder → SMT-LIB2
           ├─ raw SMT gate (size + command allowlist)
           ├─ process-wide semaphore + Z3Process
           └─ optional persist <repo_root>/.spur/solver/<solve_id>.json
```

### Crate boundaries

| Unit | Responsibility | Depends on |
|---|---|---|
| `crates/spur-solver` | Types, encoder, raw gate, `Z3Process`, **`SolverService`**, persist | `serde`, tokio process; **not** `z3` / `z3-sys` |
| `SolverService` | Process-wide ownership: binary discovery, version probe, semaphore, spawn/kill, persist root | constructed once at MCP server boot |
| `SolverMcpModule` | Thin ToolModule adapter; no private Z3 state | `Arc<SolverService>` (or equivalent inject) |
| Registry wiring (normative) | See composition sites below | `spur-core` injects the **same** `SolverService` into brain and worker modules when co-hosted |
| Z3 binary | External solver | **Operator-only:** `SPUR_Z3_BIN` then `PATH`. Agents **never** supply executable path or argv |

**Worker bridge:** Worker MCP servers receive `SolverService` via the same construction path as other worker modules (deps struct / `catalog_only` split). No separate auth protocol beyond existing worker MCP exposure; solver tools are not on the deny-list.

### Registry composition sites (normative)

`SolverMcpModule` is a **live** dispatch module (not a catalog-only placeholder). Wire it into:

| Site | Function | Notes |
|---|---|---|
| Brain live | `brain_tool_registry_with_local_projects` | Full deps; tools callable |
| Worker live | `worker_tool_registry_with_client` | Full deps; tools callable; **not** in `WORKER_DENIED_TOOL_CALLS` |
| Catalog listing | `catalog_tool_registry` | Prefer `catalog_only()` / list-shape registration so `tools/list` advertises solver tools without requiring a live Z3 handle |

**Bundled `spur mcp`:** yes — brain and worker servers that compose these registries must expose the three tools. Implementation plan names the exact `.with(SolverMcpModule::…)` call sites.

Do **not** put solver logic inside the empty default `spur-mcp` catalogs; compose the domain module the same way other live modules are composed in `spur-core`.

### Why subprocess (not FFI)

- Avoid `!Send`/`!Sync` Z3 contexts in async MCP handlers
- Crash isolation from the main `spur` process
- Packaging already heavy (DuckDB); another linked C++ lib hurts zigbuild/xwin/CI
- Matches proven z39 pattern; runner is mockable with a fake script
- Aligns with existing spur-acp kill discipline (`kill_on_drop` + process group; see `process_kill_on_drop` tests)

## MCP tool surface

All three tools are available to **brain and workers**. None belong in worker deny-lists for normal coding sessions.

### `solve_constraints`

**Purpose:** B′ typed model-finding (preferred path).

**Input (conceptual schema):**

```json
{
  "vars": [
    { "name": "workers", "type": "int_range", "min": 1, "max": 16 },
    { "name": "batch", "type": "int_range", "min": 8, "max": 128 },
    { "name": "use_cache", "type": "bool" },
    { "name": "mode", "type": "enum", "values": ["fast", "safe", "debug"] }
  ],
  "constraints": [ /* ConstraintExpr — see wire form below */ ],
  "timeout_ms": 30000,
  "persist": true
}
```

**Output:**

```json
{
  "status": "sat",
  "model": { "workers": 4, "batch": 40, "use_cache": true, "mode": "fast" },
  "duration_ms": 12,
  "solve_id": "sol_a1b2c3d4e5f67890",
  "reason": null,
  "smt": null
}
```

- `model` is present **iff** `status == "sat"`.
- Enum values in `model` are **labels** (strings), not integer indices.
- `persist: true` writes the canonical artifact and returns `solve_id`.
- Optional `smt` field may echo generated SMT for debug (off by default or truncated).

### `solve_smt`

**Purpose:** raw SMT-LIB2 escape hatch when B′ is insufficient.

**Input:** `{ "smt_lib": string, "timeout_ms"?: number, "persist"?: bool }`

**Output:** same status envelope; model parsing from preferred `(get-value (…))` when var list known, else `get-model`.

**Guards (allowlist, v1):** exceed max script bytes → `invalid_params`. Parse SMT-LIB commands (line/S-expr aware enough for top-level forms). **Allow only** a fixed set such as: `set-logic`, `set-option` (restricted keys), `declare-const`, `declare-fun`, `declare-datatype`/`declare-datatypes` (optional v1.x), `assert`, `check-sat`, `get-model`, `get-value`, `push`, `pop`, `echo` (optional). **Reject entire script** if any other top-level command appears. Runner-owned suffix may append `(check-sat)` / `(get-value …)` when absent. Never shell out beyond fixed `z3` argv (`-in`, `-memory:…`, optional `-T:…`). Agents cannot pass custom flags.

### `get_solve_result`

**Purpose:** re-fetch a persisted solve by id (brain→worker handoff).

**Input:** `{ "solve_id": "sol_…" }` — must match `^sol_[0-9a-f]{16}$` **before** any path join.  
**Output:** stored artifact payload or structured `solve_id_not_found`.

**Workers use the tool, not the filesystem path.** Do not document direct reads of `.spur/solver/*.json` as the agent API.

## B′ types and constraint AST

### Variables

| `type` | Fields | SMT sort |
|---|---|---|
| `bool` | `name` | Bool |
| `int` | `name` | Int (prefer `int_range` when bounds known) |
| `int_range` | `name`, `min`, `max` | Int + bound asserts; require `min <= max` |
| `enum` | `name`, `values: string[]` (non-empty, unique) | Int `0..n-1`; model maps back to labels |

**Name rule (surface):** `[A-Za-z_][A-Za-z0-9_]*` only.

**Identifier mangling (normative):** encoder maps each surface name to a unique SMT symbol via a fixed scheme (e.g. `v_<name>` or length-prefixed) so reserved Z3 tokens and accidental collisions cannot break scripts. Round-trip model keys must use **surface** names, not mangled symbols.

### ConstraintExpr wire form (normative) — P0

Every node is a JSON object with a `kind` tag. **Bare strings and bare numbers are not valid ConstraintExpr roots or args** (avoids ambiguity between var refs and enum labels).

```text
ConstraintExpr =
  | { "kind": "var", "name": string }                    // must be declared in vars
  | { "kind": "int", "value": integer }                  // 64-bit range; see bounds
  | { "kind": "bool", "value": boolean }
  | { "kind": "enum_label", "var": string, "label": string }  // label must be in that enum's values
  | { "kind": "op", "op": Op, "args": ConstraintExpr[] }
```

| Op class | Ops | Arity | Result sort |
|---|---|---|---|
| Compare | `eq`, `ne`, `lt`, `le`, `gt`, `ge` | 2 | Bool |
| Arith | `add`, `sub`, `mul` | ≥2 for add/mul; 2 for sub (**no** `div`) | Int |
| Bool | `and`, `or` | ≥1 | Bool |
| Bool | `not` | 1 | Bool |

**Type rules (validate before encode):**

1. Each `var` / `enum_label.var` must name a declared variable.
2. Compare/arith args must be Int-sorted (bool vars only with `eq`/`ne` against bool literals, or used inside bool ops).
3. **Enum variables are not arithmetic operands.** Only `eq` / `ne` against `enum_label` (or another enum var of the same `values` set) are allowed. No `add`/`mul`/`lt`/… on enums.
4. Top-level constraints must be Bool-sorted.
5. Integer literals must fit signed 64-bit; reject otherwise.
6. Unknown `op` or wrong arity → `invalid_params` (never pass through to Z3).

**Caps:** nest depth 32; max constraints 256; max vars 64.

**Example (indexer memory):**

```json
{
  "op": "le",
  "kind": "op",
  "args": [
    {
      "kind": "op",
      "op": "mul",
      "args": [
        { "kind": "var", "name": "workers" },
        {
          "kind": "op",
          "op": "add",
          "args": [
            { "kind": "int", "value": 48 },
            {
              "kind": "op",
              "op": "mul",
              "args": [
                { "kind": "int", "value": 2 },
                { "kind": "var", "name": "batch" }
              ]
            }
          ]
        }
      ]
    },
    { "kind": "int", "value": 512 }
  ]
}
```

### Preference / soft goals (doctrine)

v1 has no νZ. “Prefer larger X” is **not** a constraint op. Agents:

1. Solve feasibility once.
2. Optionally re-query with tighter bounds (binary search / ratcheting) using new `solve_constraints` calls.
3. Document the chosen model as one feasible point, not a proven optimum.

## Status ↔ transport mapping (normative)

| `status` | Meaning | MCP transport | `model` |
|---|---|---|---|
| `sat` | Satisfiable | Tool **result** (success) | present |
| `unsat` | No model | Tool **result** (success) | absent |
| `unknown` | Solver incomplete | Tool **result** (success) | absent |
| `timeout` | Wall clock exceeded; child killed | Tool **result** (success) | absent |
| `error` | Validation, spawn, parse, Z3 stderr, etc. | Tool **error** or result with `status=error` + `reason` | absent |

**Never** collapse `unknown` or `timeout` into `unsat`.

| Error kind | When | Transport |
|---|---|---|
| `invalid_params` | Schema, type rules, bad names, bad `solve_id` | MCP error |
| `solver_unavailable` | Z3 binary not found (install hint in message) | MCP error |
| `solve_id_not_found` | Missing artifact | MCP error or structured result |
| `output_too_large` / `parse_error` | Runner/parser failure | usually `status=error` result |
| `internal` | Unexpected | MCP error |

Do **not** use a separate `Z3NotFound` code; use `solver_unavailable`.

## Z3 process lifecycle

1. **Discover binary (boot + lazy):** `SPUR_Z3_BIN` → `PATH` lookup for `z3` (`z3.exe` on Windows). If missing → `solver_unavailable` with install hint (no silent download in v1). **Version probe:** run `z3 --version` once; store string on `SolverService` / artifacts. Unsupported platform behavior: same discovery failure path.
2. **Spawn:** fixed argv only: `z3 -in` plus:
   - `-memory:<MB>` (default **1024**)
   - optional `-T:<secs>` backstop; **wall-clock kill remains authoritative**
3. **Timeout:** wall-clock `timeout_ms` (default 30000, max 60000). Waiters on the concurrency semaphore **consume the same budget** (queue time counts).
4. **Kill / reap:** on timeout/cancel/shutdown, kill the child **process group** on Unix; on Windows use job-object / kill-tree as in spur-acp. Reap to avoid zombies. Concurrently drain **stdout and stderr** pipes (bounded).
5. **Parse:** prefer `(get-value (…))` for declared surface vars after sat; fall back to `get-model`. Tolerate multi-line `define-fun` and int/rational prints; map enum indices → labels. Unparseable after caps → `status=error` / `parse_error`.
6. **Concurrency:** **process-wide** `Semaphore` on `SolverService` (default 4). Wait until budget exhausts (no separate “busy” error required in v1).
7. **Output caps:** stdout **1 MiB**, stderr **256 KiB**; exceed → kill + `output_too_large` / `error`.

### Testability

- Abstract process runner trait; unit tests use a **fake solver script** (timeout, kill, sat/unsat fixtures) modeled on `spur-acp/tests/process_kill_on_drop.rs`.
- Real Z3 tests are **env-gated** (`SPUR_TEST_Z3=1` and binary present).
- CI: default PR CI does not require Z3; optional job or local/dev may enable it.
- Semaphore + shutdown tests in phases 0–2 (not deferred to a late phase).

## Persistence and handoff

| Mode | Behavior |
|---|---|
| `persist: false` | Ephemeral result only; `solve_id` may be omitted |
| `persist: true` | Atomic write of artifact under resolve root |

**Persist root (normative):** hosting process **repo root** (orchestrator `repo_root` / worktree root). Path: `<repo_root>/.spur/solver/<solve_id>.json`.

**Git:** `.spur/solver/` **must be gitignored** (add to repo `.gitignore` / existing `.spur` ignore policy in the implementation plan). Solver artifacts are **not** a collaboration source of truth — beads remains SoT for tasks; `solve_id` is a handoff cache only.

**Atomic write:** temp file in the same directory, `fsync`, rename. Prefer mode `0600` where OS allows.

**Quota (v1):** max **256** artifacts per repo root or max **64 MiB** total under `.spur/solver/` (whichever first); on exceed, reject new `persist:true` with clear error (no silent GC in v1).

**`solve_id` format (pinned):** `sol_` + exactly 16 lowercase hex chars. Regex: `^sol_[0-9a-f]{16}$`. Validate **before** path join (traversal-safe; reject `..`, slashes, uppercase).

### Artifact schema v1

```json
{
  "schema_version": 1,
  "solve_id": "sol_a1b2c3d4e5f67890",
  "created_at_wall": "2026-07-25T03:00:00Z",
  "z3_version": "4.13.0",
  "request": { /* canonical solve_constraints or solve_smt request */ },
  "result": {
    "status": "sat",
    "model": { },
    "duration_ms": 12,
    "reason": null
  }
}
```

`get_solve_result` returns `result` (and may include `solve_id` / `z3_version` for convenience).

**Brain→worker path:**

1. Brain calls `solve_constraints` with `persist: true`.
2. Brain embeds `solve_id` + key model fields in the worker task CONTEXT.
3. Worker calls `get_solve_result` for authoritative reload (not ad-hoc file IO).

**GC (v1):** manual / leave files; no aggressive GC. Revisit with telemetry.

**Deferred:** content-addressed store via `spur-blob-store`.

## Security

- **No** agent-supplied strings interpolated into SMT as free fragments inside the JSON path.
- Encoder owns all SMT serialization of names and literals (after mangling).
- Raw `solve_smt`: size limit; **command allowlist**; stdin-only to fixed `z3` argv.
- **`SPUR_Z3_BIN` / `PATH` are operator configuration.** Agents must never pass executable paths or Z3 flags.
- Persisted formulas/models may be sensitive; default logging must not dump full SMT/models at info level.
- **Injection / abuse test vectors (required):**
  - hostile identifiers / reserved names
  - SMT metacharacters in names (must fail validation pre-encode)
  - path traversal in `solve_id` (`../`, absolute paths)
  - oversized scripts / deep nesting / too many vars
  - enum label vs var name collisions
  - raw SMT disallowed commands (`exit`, `(echo` abuse, non-allowlisted forms)
  - model echo-spoof / unexpected Z3 stdout shapes
  - concurrent storm against semaphore + timeout budget
  - artifact quota exceed

## Resource defaults (contract)

| Parameter | Default |
|---|---|
| `timeout_ms` (if omitted) | 30000 |
| Max `timeout_ms` | 60000 |
| Max concurrent Z3 processes | 4 (process-wide) |
| Z3 `-memory:` soft cap | 1024 (MB) |
| Z3 `-T:` backstop | equal to wall timeout seconds (optional; wall kill wins) |
| Max SMT script bytes (raw) | 256 KiB |
| Max generated SMT bytes (JSON path) | 256 KiB |
| Max stdout bytes | 1 MiB |
| Max stderr bytes | 256 KiB |
| Max persisted artifacts / total size | 256 files or 64 MiB |

## Worked examples (normative fixtures)

### 1. Indexer worker pool (memory budget)

**User:** Fit pool in 512 MiB; ≥4 workers; batch 8–128; each worker `48 + 2*batch` MiB.

**Vars:** `workers` int_range 1..16, `batch` int_range 8..128.  
**Constraints:** `workers >= 4` and `workers * (48 + 2*batch) <= 512` using tagged ConstraintExpr.

**Example model (one feasible point; Z3 may return any sat assignment):** `{ "workers": 4, "batch": 40 }` satisfies equality to 512 MiB. Fixtures must assert **constraint satisfaction**, not a single golden model, unless the constraints force uniqueness.

**Worker:** constants + unit test asserting the inequality.

### 2. Frontend desktop grid (1440px) — P0 rewrite

**User:** Sidebar 240–320; rail is either 0 or 280; gutter is either 16 or 24; main ≥ 640; rail on; prefer wide sidebar.

**Vars (all int / int_range — no enum arithmetic):**

| name | type |
|---|---|
| `sidebar` | int_range 240..320 |
| `rail` | int_range 0..280 |
| `gutter` | int_range 16..24 |
| `main` | int_range 640..1440 |

**Constraints (disjunctions, not enum labels):**

- `or(eq(rail, 0), eq(rail, 280))`
- `or(eq(gutter, 16), eq(gutter, 24))`
- `eq(rail, 280)` (rail on for this solve)
- `eq(main, sub(1440, add(sidebar, rail, mul(2, gutter))))`  
  (encode nested `add`/`mul`/`sub` per wire form)
- `ge(main, 640)`

**Example model (illustrative; any sat model is valid):** `{ "sidebar": 320, "rail": 280, "gutter": 16, "main": 808 }`.

**Prefer wide sidebar:** after a sat model, optional second query with `ge(sidebar, 300)` (or ratchet); do not claim optimality. Tests check feasibility predicates, not pixel uniqueness, unless forced.

**Worker:** CSS variables + layout test using integer px.

### 3. k8s staging envelope

**User:** Replicas 2–8, HA ≥3; request 256–1024 MiB; limit = 2× request; total limits ≤ 4096.

**Solve:** `replicas >= 3` and `replicas * 2 * mem_request_mib <= 4096`.

**Example model:** `{ "replicas": 4, "mem_request_mib": 512 }`.

**Worker:** Helm values + policy test.

## Agent usage guidance

**Prefer `solve_constraints`.** Use `solve_smt` only when B′ cannot express the problem.

**Brain pattern:**

1. Encode user rules as vars + tagged ConstraintExpr.
2. `solve_constraints` with `persist: true`.
3. On `sat`, delegate coding with model + `solve_id` in CONTEXT.
4. On `unsat`, explain conflict; do not invent numbers.
5. On `timeout`/`unknown`, report honestly; tighten encoding or raise timeout within cap.
6. Soft preferences → re-query with tighter bounds.

**Worker pattern:** treat model as authoritative; do not re-invent constants; reload via `get_solve_result`.

## Implementation outline (not a full plan)

Phased build (~4–5 engineering days, estimate only):

| Phase | Scope |
|---|---|
| 0–2 | Types, validation, encoder, mangling, `Z3Process`, **`SolverService` + semaphore**, version probe, fake-solver + kill/reap tests |
| 3 | `SolverMcpModule` + inject shared service into the three composition sites |
| 4 | Shutdown coordination; optional idempotency cache (non-blocking) |
| 5 | Persist artifact schema v1, gitignore, quota + `get_solve_result` |
| 6 | `solve_smt` **allowlist** gate + docs/skill note |
| 7 | Optional blob-store (deferred) |

**Success criteria for implementation:**

1. Full tool JSON Schemas + status/error mapping tests.
2. Unit tests without Z3 (fake runner): timeout, kill, sat/unsat, semaphore budget.
3. Env-gated real Z3 tests.
4. Injection vectors enumerated and tested (incl. traversal, echo-spoof, enum collision).
5. Serde round-trips for request/response/artifact types.
6. Registry tests assert tool presence on brain, worker, and catalog list paths.
7. ConstraintExpr type-rule tests (enum arithmetic rejected; bare leaf rejected).
8. `solve_id` regex tests (reject `../`, uppercase, wrong length).
9. Atomic persist + read-back across fresh solver instance.
10. Coverage floors apply to `spur-solver` (workspace 75% / changed-line 85%).

## Naming

- Crate: `spur-solver` (scope = constraint solving for agents; documented here).
- Module: `SolverMcpModule`.
- Tools: `solve_constraints`, `solve_smt`, `get_solve_result` (optional later aliases `solver_*` via registry alias if catalog UX requires prefix grouping).

## Open questions (resolved for v1)

| Question | Resolution |
|---|---|
| Cross-process `solve_id` | Persist to `<repo_root>/.spur/solver/` with artifact schema v1 |
| Z3 distribution | PATH / `SPUR_Z3_BIN` only |
| Timeout policy | Default 30s; hard cap 60s |
| AST extensibility | Closed ops + tagged wire form; raw SMT is the escape hatch |
| Artifact GC | Manual / no GC in v1 |
| Soft “prefer” goals | Re-query doctrine; no νZ in v1 |

## References

- Microsoft Z3 / [Z3Prover/z3](https://github.com/Z3Prover/z3)
- Rust bindings (not used in v1): [prove-rs/z3.rs](https://github.com/prove-rs/z3.rs) / crates.io `z3`
- Agent-facing prior art: [z39](https://github.com/alejandroqh/z39) — learn subprocess, timeout, hybrid escape; avoid string-concat SMT, non-coding domain skew, ephemeral-only results
- SPUR MCP: `crates/spur-mcp` `ToolModule` / `ToolRegistry`; `crates/spur-core/src/mcp` (`brain_tool_registry_with_local_projects`, `worker_tool_registry_with_client`, `catalog_tool_registry`)
