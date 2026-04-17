# Brain-Driven Review Feedback Loop — Phase 2

**Date:** 2026-04-17
**Status:** Approved design, pending implementation
**Phase:** 2 of 3
**Depends on:** `2026-04-17-brain-review-feedback-loop-design.md` (Phase 1, shipped)

## Problem

Phase 1 gives the brain two review verdicts: **approve** (task is done) and **reject** (task is dead). There is no way for the brain to request refinement. The rejection is terminal — the brain has to construct a new plan to try again, losing context and round-trips. Meanwhile the human has zero visibility into the review loop in the TUI.

## Solution

Three integrated subsystems, shipped together:

- **A — Iterative refinement.** Add a third review decision `request_changes` that re-dispatches the worker with accumulated context (original task + prior diff summaries + brain feedback). Bounded by `MAX_ATTEMPTS = 3`.
- **B — Review-gated dependency dispatch.** Change the "upstream done enough to unblock dependents" rule from `AwaitingReview | Approved` to `Approved` only. Eliminates semantic drift when iteration changes upstream output.
- **C — TUI plan-review events.** Emit `PlanTaskReviewed` and `PlanTaskIterating` so the human sees brain decisions in the TUI activity log.

All three are logically coupled: A creates the need for B (iteration changes upstream mid-plan); C makes the whole loop observable. Shipping them together prevents a known-incorrect interim state.

## State Machine

### Task-Level (same 7 states, one new transition)

```
Pending → Ready → Dispatched ──→ AwaitingReview ──→ Approved
                       ↑                │
                       │                ├──→ Rejected (terminal)
                       │                │
                       │                └──→ Dispatched (request_changes)
                       │                         ↑
                       └─── new dispatch ────────┘
                            (attempt_n+1)
```

**New transition:** `AwaitingReview → Dispatched { attempt_n+1 }` via `review_task(request_changes, feedback)`.

**Updated rule:** `Pending → Ready` now requires all `depends_on` tasks to be **`Approved`** (Phase 1 treated `AwaitingReview` as done enough; Phase 2 tightens this).

### Plan-Level (unchanged from Phase 1)

Same derived statuses: `running`, `awaiting_review`, `approved`, `has_rejections`, `has_failures`, `failed`, `partial`. Counts in the response enumerate all 7 task states.

## Data Model

```rust
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    pub result: Option<DelegationResult>,   // CURRENT (latest) attempt
    pub worker_branch: Option<String>,       // CURRENT attempt's branch
    pub attempt: u32,                        // NEW: starts at 1
    pub history: Vec<AttemptRecord>,         // NEW: attempts 1..attempt-1
}

pub struct AttemptRecord {
    pub attempt: u32,
    pub worker_branch: Option<String>,       // preserved in git (not deleted)
    pub diff_summary: Option<DiffSummary>,
    pub summary: Option<String>,
    pub feedback: String,                    // brain's request_changes note
}
```

**Invariant:** `entry.result` and `entry.worker_branch` track the LATEST attempt. `history` contains attempts `1..attempt-1`. On `request_changes`: push current state into history, clear result/worker_branch, increment attempt, dispatch.

**Branch naming:** Unchanged from Phase 1 (`spur/worker-{agent}-{session_id}`). Each attempt creates a new session with a new UUID, so branch names are naturally unique — no `-attempt-N` encoding needed.

**`MAX_ATTEMPTS`:** Hardcoded to `3` in `plan.rs`. No config surface in Phase 2 (YAGNI).

## MCP Tool Surface

### `review_task` Extension (not a new tool)

Phase 1's `review_task` accepts `"approve" | "reject"`. Phase 2 adds `"request_changes"`:

**Parameters:**
- `decision`: `"approve" | "reject" | "request_changes"` (was 2 values, now 3)
- `feedback`: still optional for approve/reject, **required** for request_changes

**Response:** Same shape as Phase 1, with an additional field on iteration:

```json
{
  "task_id": "bd-ser.1",
  "decision": "request_changes",
  "new_attempt": 2,
  "new_delegation_id": "uuid-of-new-dispatch",
  "plan_status": "running",
  "counts": { "dispatched": 1, "awaiting_review": 0, ... },
  "ready_to_merge": false,
  "warnings": []
}
```

**New errors:**
- `"task is at max attempts (3); approve, reject, or leave as-is"` — when `request_changes` called on `attempt >= MAX_ATTEMPTS`
- `"request_changes requires feedback"` — feedback missing
- `"orchestrator channel closed"` — delegation dispatch failed

### Updated `review_task` Function Signature (in `plan.rs`)

```rust
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
    sink: Option<&dyn McpEventSink>,                          // NEW: Subsystem C
    delegation_tx: Option<&tokio::sync::mpsc::Sender<DelegationRequest>>,  // NEW: A + B
    task_tracker: Option<&tokio_util::task::TaskTracker>,     // NEW: A + B
) -> Result<serde_json::Value, String>
```

`handle_review_task` in `server.rs` constructs these from fields already on `McpCallbackServer` (`delegation_tx`, `task_tracker` exist in Phase 1; `event_sink` added per Subsystem C).

### `get_task_diff` Extension (optional, for Phase 2)

Add optional `attempt` parameter to inspect prior attempts:

**Parameters:**
- `plan_id` (required)
- `task_id` (required)
- `attempt` (optional, default: current). If specified, returns the diff/summary from `entry.history[attempt-1]`.

Omit for backward compatibility — current behavior (latest attempt) preserved when `attempt` is absent.

## Re-Dispatch Mechanism

Three call paths funnel dispatches through the same logic block inlined in `review_task`:

1. **Initial dispatch** (`run_plan`): tasks with no dependencies fire at plan submission. Unchanged from Phase 1.
2. **Approval cascade** (`review_task(approve)`): when a task reaches `Approved`, scan for `Pending` tasks whose deps are now all `Approved`; dispatch each.
3. **Iteration** (`review_task(request_changes)`): re-dispatch the same task with enriched context.

### Shared Dispatch Logic (inlined)

All three paths execute the same sequence under the `plan_arc` lock:

```
LOCK plan_arc
├─ Read spec + history (if iterating)
├─ Build enriched task string (see template below)
├─ Create oneshot<DelegationResult>
├─ Build DelegationRequest { task: enriched, respond_to: tx, ... }
├─ try_send(req) on delegation_tx
│   ├─ Err  → return error, state unchanged
│   └─ Ok   → proceed
├─ Update state: status = Dispatched { delegation_id }
├─ Emit PlanTaskIterating event (iteration path only)
├─ Spawn completion future in task_tracker:
│    └─ await oneshot → lock plan_arc → apply completion (same as today's run_plan)
UNLOCK plan_arc
```

**Why `try_send` not `send().await`:** Atomic under-lock. No "await-held-lock" concerns. Failure is rare (channel buffer is large) and explicit — state unchanged on failure.

### Enriched Task Template (iteration only)

```
## Original Task
{spec.task}

## Previous Attempts
Attempt 1 (branch {history[0].worker_branch}):
  Summary: {history[0].summary}
  Diff: +X/-Y across N files
  Brain feedback: {history[0].feedback}

Attempt 2 (branch {history[1].worker_branch}):
  ...

## Current Request
Apply the feedback above. You can inspect prior attempts with
`git show {branch}` if helpful.
```

No bloat cap — 3-attempt limit bounds size. Prior-attempt diffs are not embedded; the worker can run `git show` from the worktree if needed.

### Completion Future Guard

Spawned futures write results back to PlanState. Guard against stale completions (e.g., a prior attempt's result arriving after iteration started):

```rust
task_tracker.spawn(async move {
    let Ok(result) = rx.await else { return };
    let mut state = plan_arc.lock().await;
    let Some(entry) = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id) else { return };
    // Guard: only apply if we're still the expected Dispatched attempt
    match &entry.status {
        PlanTaskStatus::Dispatched { delegation_id } if delegation_id == &expected_id => {
            apply_completion_result(entry, result);
            emit_completion_event(sink, &entry);
        }
        _ => { /* stale — discard */ }
    }
});
```

### Rejection Cascade

When `review_task(reject)` runs, mark all transitively-dependent tasks as `Failed("upstream {id} rejected")`. Same scan logic as approval cascade but in reverse: BFS from the rejected task through the dependency graph.

## Event Emission (Subsystem C)

### New SpurEventBody Variants (in spur-acp)

```rust
PlanTaskReviewed {
    plan_id: String,
    task_id: String,
    decision: String,       // "approve" | "reject" | "request_changes"
    feedback: Option<String>,
    attempt: u32,
},

PlanTaskIterating {
    plan_id: String,
    task_id: String,
    attempt: u32,           // the NEW attempt number (N+1)
    delegation_id: String,
},
```

### Trait Injection (avoids circular dependency)

`spur-core` already depends on `spur-mcp`, so `spur-mcp` cannot import `FunnelHandle`. Use dependency inversion:

```rust
// crates/spur-mcp/src/events.rs (new module)
pub trait McpEventSink: Send + Sync {
    fn emit(&self, event: spur_acp::SpurEventBody);
}
```

```rust
// crates/spur-core/src/event_funnel.rs (additive)
impl spur_mcp::McpEventSink for FunnelHandle {
    fn emit(&self, event: SpurEventBody) { self.emit(event); }
}
```

`McpCallbackServer` holds `Option<Arc<dyn McpEventSink>>`. Constructed with `Some(Arc::new(funnel.clone()))` from the orchestrator-side construction site.

### Emission Points

- `PlanTaskReviewed` — at the end of every `review_task` call (approve, reject, request_changes)
- `PlanTaskIterating` — inside the `request_changes` branch, after `try_send` succeeds

### TUI Rendering

Extend the existing activity log formatter (`crates/spur-tui/src/views/activity_log.rs`) with two match arms:

```
[12:34:56] Brain approved task bd-ser.1 (attempt 1)
[12:34:57] Brain requested changes on bd-ser.2 (attempt 2): "add null check"
[12:35:02] Task bd-ser.2 iterating (attempt 2) → codex
```

Colors: approve → green, reject → red, request_changes → yellow, iterating → cyan.

## Architectural Changes

### `run_plan` Role Narrowing

In Phase 1, `run_plan` is a full plan executor — dispatches initial tasks, awaits all completions, exits when all terminal.

In Phase 2, `run_plan` becomes an **initial kickoff function**: it dispatches tasks that are Ready at plan submission, then awaits those initial completions. When its JoinSet drains, it exits.

Subsequent dispatches (approval cascade, iteration) happen inside `review_task`, which spawns its own completion futures into `task_tracker`. These outlive `run_plan`.

**Implication:** The plan's lifecycle is no longer bounded by `run_plan`. It continues as long as the brain keeps calling `review_task`. The plan's storage in `active_plans` persists until all tasks reach a terminal state (approved, rejected, or failed).

### Cleanup

When a plan reaches a fully-terminal state (all tasks Approved/Rejected/Failed), it stays in `active_plans` until explicit cleanup. Phase 3 will add a `merge_plan` tool that removes the plan after a successful merge. Until then, stale plans accumulate in memory — acceptable for a single session.

## File Change Map

| File | Change | Est. Lines |
|------|--------|------------|
| `crates/spur-acp/src/domain/events.rs` | +2 `SpurEventBody` variants | ~15 |
| `crates/spur-acp/src/domain/delegation.rs` | (no change) | 0 |
| `crates/spur-mcp/src/events.rs` | New `McpEventSink` trait module | ~10 |
| `crates/spur-mcp/src/plan.rs` | `attempt`, `history`, `AttemptRecord`, `request_changes` branch, enriched task template, rejection cascade, approval cascade, `run_plan` narrowing | ~150 |
| `crates/spur-mcp/src/server.rs` | `McpCallbackServer` holds `Option<Arc<dyn McpEventSink>>`, threaded to `review_task` | ~15 |
| `crates/spur-mcp/src/tools.rs` | Update `review_task_def`: decision enum adds `request_changes`, description mentions feedback required | ~5 |
| `crates/spur-core/src/event_funnel.rs` | `impl McpEventSink for FunnelHandle` | ~10 |
| `crates/spur-core/src/orchestrator.rs` | Pass sink to `McpCallbackServer::new` at construction site | ~5 |
| `crates/spur-tui/src/views/activity_log.rs` | Render 2 new event variants | ~20 |
| **Total** | | **~230 lines** |

## Brain Workflow (end-to-end with iteration)

```
Brain                     Orchestrator              Worker
  │                            │                       │
  ├─ submit_plan ─────────────►│                       │
  │                            ├─ dispatch task A ────►│
  │                            │◄── task A complete ───┤
  │◄─ get_plan_status ─────────┤  (AwaitingReview attempt=1)
  │                            │                       │
  ├─ get_task_diff(A) ─────────┤                       │
  │  (brain reviews)           │                       │
  ├─ review_task(A,            │                       │
  │    request_changes,        │                       │
  │    "add null check") ─────►│                       │
  │                            ├─ emit PlanTaskReviewed│ (TUI shows)
  │                            ├─ emit PlanTaskIterating
  │                            ├─ dispatch task A' ───►│
  │◄─ new_attempt: 2 ──────────┤  (Dispatched attempt=2)
  │                            │◄── task A' complete ──┤
  │                            │  (AwaitingReview attempt=2)
  │                            │                       │
  ├─ get_task_diff(A) ─────────┤                       │
  │  (brain sees attempt 2)    │                       │
  ├─ review_task(A, approve) ─►│                       │
  │                            ├─ emit PlanTaskReviewed│ (TUI shows)
  │◄─ status: approved ────────┤  (beads: done)
  │                            │                       │
  ├─ create_pr(A.worker_branch)►                       │
```

## What Is NOT In Phase 2

- `merge_plan` tool (Phase 3)
- Push notification when workers complete (Phase 3, needs MCP protocol extension)
- Plan state persistence to disk across restarts (future)
- `PlanCompleted` event (deferred to Phase 3 alongside `merge_plan`)
- Mid-worker progress events during iteration (existing `DelegationRequested`/`DelegationCompleted` suffice)
- Status bar indicator for plan review progress (Phase 3, requires status bar refactor)
- Per-plan `max_attempts` config (YAGNI)

## Backward Compatibility

- Existing Phase 1 code paths continue to work. Approve/reject decisions unchanged in behavior.
- `get_plan_status` response adds `attempt` field to each task but existing fields unchanged.
- `get_task_diff` behavior unchanged when `attempt` param omitted.
- Existing MCP clients that don't handle the new `request_changes` decision simply never use it — no breaking change.
- TUI clients that don't render the new event variants ignore them (forward-compatible tagged enum).
