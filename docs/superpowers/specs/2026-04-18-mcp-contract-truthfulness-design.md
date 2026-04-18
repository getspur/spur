# MCP Contract Truthfulness — T1 Design Spec

**Date:** 2026-04-18
**Author:** Codex (L9 Rust staff pass)
**Scope tier:** T1 (of T1 → T3 → T2 convergence roadmap)
**Source RCA:** `docs/rca/2026-04-18-brain-worker-beads-mcp-journey.md` (Phase 2 §2.5)
**Status:** design — awaiting review

---

## 1. Purpose

Restore truthfulness at the MCP tool surface so the brain's beliefs about its delegation tools match runtime behavior. Every parameter and tool reaches one of three states — **honored**, **rejected-with-error**, or **removed from schema**. No parameter is accepted and silently ignored. No tool reports `Failed` as `success`.

This is the prerequisite work for T2 (coherence — Beads atomicity, event-stream parity) and T3 (domain model — hierarchy split, poll parity). T2 and T3 are intentionally out of scope here.

## 2. Background

The RCA Phase 2 grounding confirmed seven root causes (R1–R7) and added four adversarial findings (A1–A4). Within that set, five are truthfulness violations localized to `spur-mcp` + `spur-core`:

- **R1** `context_files` accepted by schema + server, dropped by `orchestrator.rs::execute_delegation` (param renamed `_context_files`).
- **R2/A2** `cancel_delegation`, `report_progress`, `get_session_cost` stubbed in orchestrator. Worse than RCA initially stated: `handle_cancel_delegation` returns JSON-RPC `success` with the stub-error string as body text.
- **R3/A5** `delegate_parallel` clones top-level `issue_id` into every per-task `DelegationRequest`. N workers race on one PM identity.
- **R7** `get_issue` / `update_issue` advertise a `source` parameter (backend override) that handlers never read.
- **A1** `delegate_parallel` per-task schema has no `context_files` field; server hardcodes `Vec::new()`.
- **A3** `delegate_parallel` clones top-level `delegation_plan` into every per-task `DelegationRequest`. Corrupts reviewer mismatch detection (`orchestrator.rs:2767-2787`).

All five defects are schema/handler contract drift. T1 closes them within one crate boundary.

## 3. Scope

### 3.1 In scope

- Schema edits in `crates/spur-mcp/src/tools.rs`.
- Handler edits in `crates/spur-mcp/src/server.rs`.
- `execute_delegation` signature + body changes in `crates/spur-core/src/orchestrator.rs`.
- New tests in `crates/spur-mcp/tests/` (or adjacent test modules).

### 3.2 Out of scope (explicit non-goals)

- Real wiring of any removed tool. If `report_progress` comes back later as a `BrainProgress` event emitter, that is a separate spec.
- Multi-backend PM routing (the `source` parameter reinstated). Future spec once `PmService` supports more than one backend simultaneously.
- Beads atomicity (R4), `IssueUpdated` emission on success (A4) — T2.
- Domain-model split `parent`/`children` (R5), poll parity (R6) — T3.
- ACP protocol evolution for structured `context_files` transport.

### 3.3 Non-breaking clauses

- `delegate_to_worker` continues to accept the same request shape. Only semantic change: `context_files` now reaches the worker prompt.
- `delegate_parallel` top-level `issue_id` is **removed**. This is a schema-level breaking change for any caller passing it. Mitigated by migration note in §7.

## 4. Design decisions

Three decisions, one per RCA defect cluster. Each is the MCTS-winning posture from the Q1/Q2/Q3 brainstorm.

### 4.1 Decision T1.1 — Control-plane tools truthfulness

**`cancel_delegation`** — *kept in schema, error-stub with honest JSON-RPC error.*

- Handler continues to forward `__cancel_delegation` to orchestrator.
- When orchestrator returns `DelegationResult { status: Failed { error }, summary: None, .. }`, server responds with `JsonRpcResponse::error(-32601, format!("cancel_delegation: {error}"))`. Never `success`.
- Orchestrator `__cancel_delegation` branch in `execute_delegation` retained — real implementation lands in a later spec.

**`report_progress`** — *removed from schema.*

- Delete `report_progress_def()` from tool list.
- Delete `handle_report_progress()` from server.
- Delete `__progress` branch from `execute_delegation`.
- Future: may return as a real `SpurEventBody::BrainProgress` emitter; separate spec if/when that is prioritized.

**`get_session_cost`** — *removed from schema.*

- Delete `get_session_cost_def()` from tool list.
- Delete `handle_get_session_cost()` from server.
- Delete `__session_cost` branch from `execute_delegation`.
- Per-delegation cost remains available via `DelegationResult.estimated_cost_usd`; brain aggregates client-side if cross-delegation cost is needed.

### 4.2 Decision T1.2 — `context_files` reaches the worker

**Mechanism:** path injection into the worker prompt string, inside `execute_delegation`. No file I/O in `spur-mcp`. No ACP schema change.

**Prompt format:**

```
## Relevant Files

The following files were declared as relevant by the caller. Open them with your Read tool as needed.

- path/to/file/one.rs
- path/to/file/two.rs

## Task

<original task body>
```

**Empty-list short-circuit:** when `context_files.is_empty()`, the task string is passed through unchanged (no `## Relevant Files` header).

**Helper:** new `fn format_worker_task(task: &str, context_files: &[String]) -> String` in `spur-core/src/orchestrator.rs`. Pure function, unit-testable.

**`delegate_to_worker`** — schema already has `context_files`; no change needed.

**`delegate_parallel`** — per-task object schema **gains `context_files`** (string array, optional). `handle_delegate_parallel` stops hardcoding `Vec::new()`; parses per-task and forwards into each `DelegationRequest`.

**`execute_delegation` signature:** rename `_context_files: Vec<String>` → `context_files: Vec<String>`. Call `format_worker_task(&task, &context_files)` to produce the final prompt before worker spawn.

### 4.3 Decision T1.3 — `delegate_parallel` per-task identity + audit

**`issue_id`** — move to per-task, remove top-level.

- `tasks[].issue_id` (string, optional).
- Top-level `issue_id` field removed from `delegate_parallel` schema.
- Each per-task `DelegationRequest` carries its own `issue_id` (or `None`).

**`delegation_plan`** — add per-task, keep top-level, stop cloning.

- `tasks[].delegation_plan` (object, optional) — per-task reviewer mismatch input.
- Top-level `delegation_plan` retained for batch decomposition documentation; description updated to clarify "batch-level rationale; per-task plans take precedence for reviewer mismatch checks."
- `handle_delegate_parallel` **does not clone** top-level into per-task requests. Per-task `DelegationRequest.delegation_plan = task_obj.get("delegation_plan").and_then(..).unwrap_or(None)`. Top-level is parsed from the args and may optionally be logged at `info!` level for audit; it never flows into any `DelegationRequest`. Logging is not required by this spec.

**Uniqueness validation (HX2):**

- If `tasks[]` contains two or more entries with the same non-`None` `issue_id`, return `JsonRpcResponse::invalid_params(id, "delegate_parallel: issue_id values must be unique across tasks")` before any `DelegationRequest` is sent.

### 4.4 Decision T1.4 — `source` parameter removed from PM tool schemas

- Delete `source` property from `tools.rs::get_issue_def()` `inputSchema.properties`.
- Delete `source` property from `tools.rs::update_issue_def()` `inputSchema.properties`.
- Handlers already ignore the field; no server-side change needed.
- Future multi-backend spec will reintroduce with real routing.

## 5. Schema after T1

### 5.1 `delegate_to_worker`

Unchanged in shape. Semantics: `context_files` now reaches the worker prompt.

### 5.2 `delegate_parallel`

```jsonc
{
  "type": "object",
  "properties": {
    "tasks": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "agent":           { "type": "string" },
          "task":            { "type": "string" },
          "context_files":   { "type": "array", "items": { "type": "string" } },
          "issue_id":        { "type": "string" },
          "delegation_plan": { "type": "object" }
        },
        "required": ["agent", "task"]
      },
      "description": "List of tasks to delegate in parallel. Each task tracks its own issue and carries its own delegation_plan for per-worker reviewer mismatch detection."
    },
    "delegation_plan": {
      "type": "object",
      "description": "Batch-level decomposition rationale. Documents why these N subtasks together and how they are independent. Per-task delegation_plan (inside tasks[]) takes precedence for reviewer mismatch checks."
    }
  },
  "required": ["tasks"]
}
```

### 5.3 `get_issue`, `update_issue`

`source` property removed. `id` remains required.

### 5.4 Removed tools

- `report_progress` — removed from `tools/list`.
- `get_session_cost` — removed from `tools/list`.

## 6. Invariants (CI-enforceable post-T1)

Each invariant has an assertion form suitable for a test or lint.

**INV-1** — Every key declared in a `tools.rs` `inputSchema.properties` block has a matching `args.get("<key>")` call in the corresponding handler in `server.rs`. (Enforce via a grep-based lint or a reflective test over the tool list.)

**INV-2** — `crates/spur-core/src/orchestrator.rs::execute_delegation`'s `context_files` parameter is structurally consumed in prompt assembly. Enforced by compiler via absence of `_`-prefixed unused warning.

**INV-3** — For `delegate_parallel`, per-task `issue_id` values are either `None` or pairwise distinct across one batch. Enforced at runtime by `handle_delegate_parallel` pre-dispatch validation; asserted by unit test.

**INV-4** — For `delegate_parallel`, per-task `DelegationRequest.delegation_plan` equals the per-task schema value (or `None`). It is never the top-level `delegation_plan`. Enforced by integration test.

**INV-5** — When `execute_delegation` returns `DelegationResult { status: Failed, .. }` via the `__*` stub guard for `__cancel_delegation`, the server's `handle_cancel_delegation` converts this to a JSON-RPC `error(-32601)` response, not `success`. Enforced by handler unit test.

## 7. Migration / compatibility

### 7.1 Protocol-breaking changes (tools disappear or response shape changes)

- `report_progress` tool disappears from `tools/list`. Brain system prompts that statically enumerate tools must drop the reference. Runtime effect if brain still calls it: `error -32601 Unknown tool: report_progress` (already the default in `server.rs`).
- `get_session_cost` — same as above.

### 7.2 Semantic-breaking changes (same protocol, different meaning)

- `cancel_delegation` — same tool, same protocol shape, but response class flips from `success` (with error text in body) to `error(-32601, "cancel_delegation: ...")`. Callers interpreting the response as "did the cancel succeed?" must now check for error code `-32601` rather than parsing the body text.

### 7.3 Schema-shape changes, runtime-silent

- `delegate_parallel` top-level `issue_id` — field removed from schema. If a brain callsite still passes it, JSON-RPC tolerates unknown fields and the value is silently ignored (same behavior as pre-T1 for that field's effect, because the post-T1 server never reads it).
- `get_issue` / `update_issue` top-level `source` — same silent-ignore behavior as before T1 (the field was never read pre-T1 either); schema becomes honest.

### 7.4 Non-breaking changes

- `delegate_to_worker` — same request shape; `context_files` now effective.
- `delegate_parallel` per-task adds three optional fields; existing callers passing only `{agent, task}` continue to work.

### 7.5 Brain prompt audit

Before merge, grep `crates/spur-acp/src/agents/defaults.rs` and any shipped brain system-prompt templates for static references to `report_progress`, `get_session_cost`, or the top-level `delegate_parallel.issue_id` field. Update in the same change.

## 8. Testing strategy

### 8.1 Unit tests (per decision)

- **T1.1** `handle_cancel_delegation`: mock orchestrator returning `DelegationResult { status: Failed { error: "x" }, summary: None, .. }` → response is `JsonRpcResponse::error(-32601, _)`.
- **T1.2** `format_worker_task`: empty list → task unchanged; one path → `## Relevant Files` section with one bullet; multiple paths → ordered bullets; whitespace-only task body still gets section prepended.
- **T1.3** `handle_delegate_parallel` uniqueness: two tasks with same non-`None` `issue_id` → `invalid_params`; two tasks with distinct `issue_id` → proceeds; one task with `issue_id`, others with `None` → proceeds.
- **T1.4** n/a (removal only; enforced by schema shape).

### 8.2 Integration tests

- `delegate_parallel` with two tasks, each with distinct `context_files`, `issue_id`, and `delegation_plan` → mock orchestrator's `delegation_tx` receives two `DelegationRequest` values whose per-field values match the per-task inputs (not `Vec::new()`, not cloned from sibling or top-level).
- `delegate_parallel` with top-level `delegation_plan` present but no per-task plans → each `DelegationRequest.delegation_plan` is `None`; top-level value is NOT propagated.

### 8.3 Regression tests to delete

- Any test asserting `delegate_parallel` passes `Vec::new()` for `context_files`.
- Any test asserting top-level `delegation_plan` flows to per-task workers.
- Any test listing `report_progress` or `get_session_cost` as present in `tools/list`.

### 8.4 Tool-list schema-handler parity enforcement (INV-1)

Long-run enforcement of INV-1 ("every `inputSchema.properties` key has a matching `args.get()` read"). Two candidate mechanisms — exact choice is deferred to the implementation plan:

- (a) A static analysis test that parses the handler source file and diffs the set of `args.get("...")` string literals against each schema's property keys.
- (b) A refactor where each handler declares a typed request struct derived from the schema, with `#[serde(deny_unknown_fields)]` catching schema-handler drift at call time.

(a) is cheaper for T1. (b) is the durable form. `writing-plans` decides which to use based on Rust/serde ergonomics at implementation time; T1 merges only if at least one is in place.

## 9. Rollout

Single PR. No feature flag. Changes are coherent only as a set:

1. Schema edits.
2. Handler edits.
3. Orchestrator edits.
4. Test updates.
5. Brain prompt audit.

Reverting is clean: one PR revert restores prior (broken) state.

## 10. Implementation notes for `writing-plans`

Design-level decisions are locked. Two implementation details are deferred to plan-writing:

- **N-IMPL-1** — INV-1 enforcement mechanism: static-analysis test (option a in §8.4) vs typed request structs with `serde(deny_unknown_fields)` (option b). Plan writer picks one; T1 ships with at least one.
- **N-IMPL-2** — `format_worker_task` call-site: insert at `execute_delegation` entry vs at an intermediate worker-prompt assembly step. Depends on whether `original_task: String` is passed through unchanged to ACP `prompt()` or assembled downstream. Plan writer verifies the call chain and selects the correct site; spec requirement is only that the helper's output reaches the worker prompt.

## 11. Appendix — MCTS record

Phase 2 of the RCA established confidence ≥0.90 on every R# claim. The three design decisions above were each selected via six-round sequential-thinking rollouts with branch pruning. Full audit trail:

- Q1 (control-plane tools): posture D winning; branches A (remove all), B (all error-stubs), β3 (rewire report_progress now) pruned on scope creep / regret asymmetry.
- Q2 (context_files): S1+P1+H4 winning; S2 (server file I/O) pruned on wrong-cwd correctness, S3 (ACP schema) on scope, P2/P3 on capability loss.
- Q3 (parallel fields): I1+D1+HX2 winning; I3/I4 pruned on T3.1 dependency, D3 is the current bug, HX3 (auto-derive) rejected as silent inference.

Full rounds in the conversation history preceding this spec.
