# Z3 Constraint Solver for Coding Agents — Design

**Status:** Approved (brainstorming)  
**Date:** 2026-07-25  
**Crate (proposed):** `spur-solver`  
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
| Timeout | Default/hard-oriented **60s** uniform (per-call override lower ok) |
| Concurrency | Cap concurrent Z3 processes (default **4**) |
| Constraint language | **Closed** AST ops only; no string-concat into SMT from agent fragments |
| Optimization (νZ) | Out of v1; agents may re-query with tighter bounds |

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
    → SolverMcpModule (ToolModule)
      → spur-solver
           ├─ types + validate
           ├─ B′ encoder → SMT-LIB2
           ├─ raw SMT gate (size + command deny-list)
           └─ Z3Process (spawn, timeout, kill, parse)
                → optional persist .spur/solver/<solve_id>.json
```

### Crate boundaries

| Unit | Responsibility | Depends on |
|---|---|---|
| `crates/spur-solver` | Request/response types, B′→SMT encoder, Z3 runner, model parse, optional file persist | `serde`, tokio process; **not** `z3` / `z3-sys` |
| `SolverMcpModule` | Tool definitions + dispatch (lives in `spur-solver` like `spur-graph`/`spur-analyst` modules) | `spur-mcp::ToolModule` |
| Registry wiring | Register module in **both** `brain_tool_registry` and `worker_tool_registry` | `spur-core` composition sites |
| Z3 binary | External solver | `SPUR_Z3_BIN`, then `PATH` |

Do **not** put solver logic inside empty default `spur-mcp` catalogs; follow domain-module composition (`GraphMcpModule`, `AnalystMcpModule`).

### Why subprocess (not FFI)

- Avoid `!Send`/`!Sync` Z3 contexts in async MCP handlers
- Crash isolation from the main `spur` process
- Packaging already heavy (DuckDB); another linked C++ lib hurts zigbuild/xwin/CI
- Matches proven z39 pattern; runner is mockable with a fake script

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
  "constraints": [ /* ConstraintExpr */ ],
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
- `persist: true` writes the canonical result to disk and returns `solve_id`.
- Optional `smt` field may echo generated SMT for debug (off by default or truncated).

### `solve_smt`

**Purpose:** raw SMT-LIB2 escape hatch when B′ is insufficient.

**Input:** `{ "smt_lib": string, "timeout_ms"?: number, "persist"?: bool }`

**Output:** same status envelope; model parsing best-effort from `get-model` / `get-value` output.

**Guards:** max script bytes; reject or strip dangerous/irrelevant commands (implementation lists `set-option` abuse, shell escapes — never pass through unchecked OS interaction; Z3 is pure solver stdin).

### `get_solve_result`

**Purpose:** re-fetch a persisted solve by id (brain→worker handoff).

**Input:** `{ "solve_id": "sol_…" }`  
**Output:** stored result or structured `not_found` error.

## B′ types and constraint AST

### Variables

| `type` | Fields | SMT sort |
|---|---|---|
| `bool` | `name` | Bool |
| `int` | `name` | Int (prefer `int_range` when bounds known) |
| `int_range` | `name`, `min`, `max` | Int + bound asserts |
| `enum` | `name`, `values: string[]` | Int `0..n-1` + model maps back to labels |

**Name rule:** `[A-Za-z_][A-Za-z0-9_]*` only (encoder-safe; no SMT metacharacters).

### Constraint ops (closed set)

| Class | Ops |
|---|---|
| Compare | `eq`, `ne`, `lt`, `le`, `gt`, `ge` |
| Arith | `add`, `sub`, `mul` (**no** `div` in v1) |
| Bool | `and`, `or`, `not` |
| Leaves | var name, int literal, bool literal, enum label string |

Unknown `op` → validation error (do not pass through to Z3).

**Caps (defaults, configurable later):** nest depth 32; max constraints 256; max vars 64.

### Status taxonomy (normative)

| Status | Meaning |
|---|---|
| `sat` | Satisfiable; `model` present |
| `unsat` | No model under constraints |
| `unknown` | Solver returned unknown (incomplete) |
| `timeout` | Wall clock exceeded; process killed |
| `error` | Validation, spawn, parse, or Z3 error |

**Never** collapse `unknown` or `timeout` into `unsat`. Agents make product decisions on this field.

## Z3 process lifecycle

1. **Discover binary:** `SPUR_Z3_BIN` → `PATH` lookup for `z3`. If missing → `solver_unavailable` / `Z3NotFound` with install hint (no silent download in v1).
2. **Spawn:** `z3 -in` (or equivalent), stdin = full SMT script ending with `(check-sat)` and model extraction commands.
3. **Timeout:** wall-clock `timeout_ms` capped by server max (60s default ceiling).
4. **Kill:** kill process group on timeout/cancel/shutdown; no zombies (match spur-acp kill-on-drop discipline).
5. **Parse:** status line + model assignments; map enum indices → labels for JSON path.
6. **Concurrency:** global semaphore (default 4). Excess calls wait or fail with explicit busy/error policy (spec implementation: wait with same timeout budget preferred).
7. **Stdout cap:** reject/truncate pathological output; treat as `error` if unparseable after cap.

### Testability

- Abstract process runner trait; unit tests use a **fake solver script** (timeout, kill, sat/unsat fixtures).
- Real Z3 tests are **env-gated** (e.g. `SPUR_TEST_Z3=1` and binary present).
- CI: default PR CI does not require Z3; optional job or local/dev may enable it.

## Persistence and handoff

| Mode | Behavior |
|---|---|
| `persist: false` | In-memory only for the process; no `solve_id` required |
| `persist: true` | Write `.spur/solver/<solve_id>.json` (canonical result JSON) |

**`solve_id` format:** `sol_` + 16 hex chars (or UUID compact).

**Brain→worker path:**

1. Brain calls `solve_constraints` with `persist: true`.
2. Brain embeds `solve_id` + key model fields in the worker task CONTEXT.
3. Worker may call `get_solve_result` to re-load authoritative model (avoids drift).

**GC (v1):** manual / leave files; no aggressive GC (match delegation artifact policy). Revisit with telemetry.

**Deferred:** content-addressed store via `spur-blob-store`.

## Error model (agent-facing)

| Code / kind | When |
|---|---|
| `invalid_params` | Schema fail, bad names, unknown op, empty vars |
| `solver_unavailable` | Z3 binary not found |
| `timeout` | Wall clock exceeded |
| `unsat` | (status, not always transport error — prefer 200-like tool result with `status`) |
| `solve_id_not_found` | `get_solve_result` miss |
| `output_too_large` / `parse_error` | Runner/parser failure |
| `internal` | Unexpected |

Tool results should prefer **structured success payloads with `status`** for sat/unsat/unknown/timeout so agents do not treat unsat as a transport failure. Validation and missing Z3 remain hard errors.

## Security

- **No** agent-supplied strings interpolated into SMT as free fragments inside the JSON path.
- Encoder owns all SMT serialization of names and literals.
- Raw `solve_smt`: size limit; treat input as data to Z3 stdin only.
- Injection test vectors (required in implementation plan): hostile identifiers, SMT metacharacters in names, oversized scripts, deep nesting, reserved names.

## Resource defaults (contract)

| Parameter | Default |
|---|---|
| `timeout_ms` (if omitted) | 30000 |
| Max `timeout_ms` | 60000 |
| Max concurrent Z3 processes | 4 |
| Max SMT script bytes (raw) | 256 KiB |
| Max stdout bytes | 1 MiB |

## Worked examples (normative fixtures)

### 1. Indexer worker pool (memory budget)

**User:** Fit pool in 512 MiB; ≥4 workers; batch 8–128; each worker `48 + 2*batch` MiB.

**Solve:** `workers`, `batch` with `workers >= 4` and `workers * (48 + 2*batch) <= 512`.

**Example model:** `{ "workers": 4, "batch": 40 }` (exactly 512 MiB).

**Worker:** constants + unit test asserting the inequality.

### 2. Frontend desktop grid (1440px)

**User:** Sidebar 240–320; rail 0 or 280; gutter 16 or 24; main ≥ 640; prefer wide sidebar; rail on.

**Solve:** geometry identity `main = 1440 - sidebar - rail - 2*gutter` with bounds.

**Example model:** `{ "sidebar": 320, "rail": "280", "gutter": "16", "main": 808 }`.

**Worker:** CSS variables + layout test.

### 3. k8s staging envelope

**User:** Replicas 2–8, HA ≥3; request 256–1024 MiB; limit = 2× request; total limits ≤ 4096.

**Solve:** `replicas >= 3` and `replicas * 2 * mem_request_mib <= 4096`.

**Example model:** `{ "replicas": 4, "mem_request_mib": 512 }`.

**Worker:** Helm values + policy test.

## Agent usage guidance

**Prefer `solve_constraints`.** Use `solve_smt` only when B′ cannot express the problem.

**Brain pattern:**

1. Encode user rules as vars + constraints.
2. `solve_constraints` with `persist: true`.
3. On `sat`, delegate coding with model + `solve_id` in CONTEXT.
4. On `unsat`, explain conflict; do not invent numbers.
5. On `timeout`/`unknown`, report honestly; tighten encoding or raise timeout within cap.

**Worker pattern:** treat model as authoritative; do not re-invent constants; optional `get_solve_result`.

## Implementation outline (not a full plan)

Phased build (~4–5 engineering days, estimate only):

| Phase | Scope |
|---|---|
| 0–2 | Types, validation, encoder, `Z3Process` + fake-solver tests |
| 3 | `SolverMcpModule` + brain/worker registry wiring |
| 4 | Concurrency semaphore + shutdown |
| 5 | Persist `solve_id` + `get_solve_result` |
| 6 | `solve_smt` + docs/skill note |
| 7 | Optional blob-store (deferred) |

**Success criteria for implementation:**

1. Full tool JSON Schemas + error taxonomy in plan/tests.
2. Unit tests without Z3 (fake runner).
3. Env-gated real Z3 tests.
4. Injection vectors enumerated and tested.
5. Serde round-trips for request/response/artifact types.
6. Registry tests assert tool presence on brain and worker catalogs.
7. Coverage floors apply to `spur-solver` (workspace 75% / changed-line 85%).

## Naming

- Crate: `spur-solver` (scope = constraint solving for agents; documented here).
- Module: `SolverMcpModule`.
- Tools: `solve_constraints`, `solve_smt`, `get_solve_result` (optional later aliases `solver_*` via registry alias if catalog UX requires prefix grouping).

## Open questions (resolved for v1)

| Question | Resolution |
|---|---|
| Cross-process `solve_id` | Persist to `.spur/solver/` from day one of persist feature |
| Z3 distribution | PATH / `SPUR_Z3_BIN` only |
| Timeout policy | Uniform 60s max |
| AST extensibility | Closed ops; raw SMT is the escape hatch |
| Artifact GC | Manual / no GC in v1 |

## References

- Microsoft Z3 / [Z3Prover/z3](https://github.com/Z3Prover/z3)
- Rust bindings (not used in v1): [prove-rs/z3.rs](https://github.com/prove-rs/z3.rs) / crates.io `z3`
- Agent-facing prior art: [z39](https://github.com/alejandroqh/z39) — learn subprocess, timeout, hybrid escape; avoid string-concat SMT, non-coding domain skew, ephemeral-only results
- SPUR MCP: `crates/spur-mcp` `ToolModule` / `ToolRegistry`; `crates/spur-core/src/mcp` registry composition
