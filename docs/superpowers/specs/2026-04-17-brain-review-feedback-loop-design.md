# Brain-Driven Review Feedback Loop

**Date:** 2026-04-17
**Status:** Approved design, pending implementation
**Phase:** 1 of 3

## Problem

When a worker agent completes a plan task, the orchestrator commits changes to the worker branch then **deletes the branch** (`git branch -D`). Approved work is lost from git. The brain has no mechanism to review worker output, approve/reject it, or trigger iteration. Beads issue status stays `in_progress` after successful completion — never transitions to `done`.

## Solution

Add policy-agnostic review infrastructure: two MCP tools (`get_task_diff`, `review_task`), three new `PlanTaskStatus` states (`AwaitingReview`, `Approved`, `Rejected`), and a `detach_worktree()` method that preserves approved branches for future merge.

The brain's review **behavior** (how aggressively it reviews, whether it escalates to human) is controlled by system prompt configuration, not by infrastructure code. The same tools support autonomous review (brain-only), supervised review (brain + human), and gated review (brain escalates high-risk tasks).

## Phasing

| Phase | Scope | Description |
|-------|-------|-------------|
| **1 (this spec)** | ~190 lines, 6 files | Review states, get_task_diff, review_task, detach_worktree, beads sync |
| 2 | ~120 lines | `request_changes` decision, re-dispatch with feedback, attempt tracking |
| 3 | ~100 lines | `merge_plan` tool, cherry-pick approved branches, create_pr integration |

## State Machine

### Task-Level (7 states)

```
Pending → Ready → Dispatched ──→ AwaitingReview ──→ Approved
                       │                │
                       │                └──→ Rejected
                       │                │
                       │                └──→ Dispatched (Phase 2: request_changes)
                       │
                       └──→ Failed (terminal, preserves partial diff)
```

**Transition rules:**

| From | To | Trigger |
|------|----|---------|
| Pending | Ready | All `depends_on` tasks reach Dispatched or beyond |
| Ready | Dispatched | Plan executor sends DelegationRequest |
| Dispatched | AwaitingReview | Worker succeeds (DelegationStatus::Success or Modified) |
| Dispatched | Failed | Worker fails (any non-success DelegationStatus) |
| AwaitingReview | Approved | Brain calls `review_task(approve)` |
| AwaitingReview | Rejected | Brain calls `review_task(reject)` |
| AwaitingReview | Dispatched | Phase 2: brain calls `review_task(request_changes)` |

**Eliminated states** (from earlier proposal):
- `Completed` — merged into `AwaitingReview` (no information lost, one fewer transient state)
- `ChangesRequested` — deferred to Phase 2 (handled as Dispatched with bumped attempt_n)

### Plan-Level (derived from task counts)

| Status | Condition |
|--------|-----------|
| `running` | Any task is Dispatched |
| `awaiting_review` | All workers done, some tasks not yet reviewed |
| `approved` | All tasks Approved |
| `has_rejections` | All reviewed, some Rejected |
| `failed` | All tasks Failed |

Plan status response includes counts for brain decision-making:

```json
{
  "status": "awaiting_review",
  "counts": {
    "total": 4,
    "pending": 0,
    "ready": 0,
    "dispatched": 0,
    "awaiting_review": 2,
    "approved": 1,
    "rejected": 0,
    "failed": 1
  },
  "all_workers_done": true,
  "ready_to_merge": false
}
```

## MCP Tool Surface (2 new tools)

### `get_task_diff(plan_id, task_id)`

Returns the full unified diff for a task so the brain can review it.

**Parameters:**
- `plan_id` (string, required): Plan identifier
- `task_id` (string, required): Task identifier within the plan

**Response:**

```json
{
  "task_id": "bd-ser.1",
  "status": "awaiting_review",
  "agent": "claude-code-acp",
  "worker_branch": "spur/worker-claude-code-acp-e4091831",
  "task_description": "Add example tasks to TUI splash screen...",
  "diff": "diff --git a/crates/spur-tui/src/views/dashboard.rs ...",
  "diff_summary": { "files_changed": 1, "insertions": 19, "deletions": 1 },
  "summary": "Added 3 example tasks below splash screen..."
}
```

**Behavior:**
- Works on `AwaitingReview`, `Approved`, `Rejected`, and `Failed` tasks (brain can inspect failures)
- Returns full unified diff always (no truncation — brain has `diff_summary` from `get_plan_status` to gauge size before requesting)
- Errors: unknown plan/task, task still Dispatched (diff not available), task Pending/Ready (never dispatched)

### `review_task(plan_id, task_id, decision, feedback?)`

Brain submits its review verdict for a completed task.

**Parameters:**
- `plan_id` (string, required): Plan identifier
- `task_id` (string, required): Task identifier
- `decision` (string, required): `"approve"` or `"reject"` (Phase 2 adds `"request_changes"`)
- `feedback` (string, optional): Review notes (required context for reject, optional for approve)

**Response:**

```json
{
  "task_id": "bd-ser.1",
  "decision": "approve",
  "plan_status": "awaiting_review",
  "counts": { "awaiting_review": 1, "approved": 3, "rejected": 0, "failed": 0 },
  "ready_to_merge": false,
  "warnings": []
}
```

**Side effects:**
- Primary (always succeeds): PlanTaskStatus update in memory
- Secondary (non-blocking): Beads issue status update + comment
  - Approve: `status: "done"`, comment: `"Brain approved: {feedback}"`
  - Reject: `status: "open"`, comment: `"Brain rejected: {feedback}"`
  - If beads update fails, recorded in `warnings` array — does not fail the review

**Errors:** unknown plan/task, task not in `AwaitingReview`, invalid decision string

## Infrastructure Changes

### 1. `detach_worktree()` — spur-worktree/manager.rs

New method alongside existing `remove_worktree()`:

```rust
pub async fn detach_worktree(&mut self, session_id: &SessionId) -> Result<String> {
    let session_str = session_id.to_string();
    let info = self.active.remove(&session_str)
        .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;
    let path_str = info.path.to_str()
        .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
    self.run_git(&["worktree", "remove", path_str, "--force"], None).await
        .with_context(|| format!("failed to remove worktree at {path_str}"))?;
    // Branch intentionally NOT deleted — preserved for brain review + merge
    Ok(info.branch)
}
```

### 2. Three-way cleanup — spur-core/orchestrator.rs

`apply_worktree_cleanup` becomes a three-way dispatch returning `Option<String>`:

| Condition | Action | Returns |
|-----------|--------|---------|
| `should_preserve_worktree` (Rejected, TimedOut-Reject) | Keep dir + branch | `None` |
| `should_commit_worker_diff` (Success, Modified) | Commit, detach (remove dir, keep branch) | `Some(branch_name)` |
| Neither (Failed, Retry) | Remove everything | `None` |

The returned branch name flows into `DelegationResult.worker_branch`.

### 3. `worker_branch` on DelegationResult — spur-acp/delegation.rs

```rust
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
    pub diff_summary: Option<DiffSummary>,
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
    pub worker_branch: Option<String>,  // NEW: preserved branch for merge
}
```

### 4. Plan executor update — spur-mcp/plan.rs

On DelegationResult received:
- `Success` | `Modified` → `PlanTaskStatus::AwaitingReview`, store `result` + `worker_branch`
- Any failure → `PlanTaskStatus::Failed`, store `result` (partial diff for inspection)

`AwaitingReview` is terminal for the Phase 1 executor — `run_plan()` exits when all tasks are `AwaitingReview` or `Failed`.

### 5. PlanTaskEntry update — spur-mcp/plan.rs

```rust
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    pub result: Option<DelegationResult>,
    pub worker_branch: Option<String>,
}
```

## Dependency Behavior (Phase 1 limitation)

In Phase 1, dependent tasks dispatch when upstream workers **complete** (succeed), not when upstream tasks are **approved**. This means downstream tasks may start before upstream review — a semantic gap accepted for simplicity.

Phase 2 introduces review-gated dispatch: dependent tasks wait for upstream `Approved` status before becoming `Ready`.

## File Change Map

| File | Change | Est. Lines |
|------|--------|------------|
| `crates/spur-worktree/src/manager.rs` | `detach_worktree()` method | ~15 |
| `crates/spur-acp/src/domain/delegation.rs` | `worker_branch` field on `DelegationResult` | ~3 |
| `crates/spur-core/src/orchestrator.rs` | Three-way cleanup, return branch name, thread to result | ~20 |
| `crates/spur-mcp/src/plan.rs` | New states, AwaitingReview transition, `review_task()`, enriched status | ~80 |
| `crates/spur-mcp/src/tools.rs` | `get_task_diff_def()` + `review_task_def()` definitions | ~35 |
| `crates/spur-mcp/src/server.rs` | `handle_get_task_diff()` + `handle_review_task()` + dispatch arms | ~35 |
| **Total** | | **~190** |

## Brain Workflow (end-to-end)

```
Brain                          Orchestrator                    Worker
  │                                │                              │
  ├─ submit_plan(tasks) ──────────►│                              │
  │                                ├─ dispatch task A ───────────►│
  │                                ├─ dispatch task B ───────────►│
  │                                │                              │
  │                                │◄── task A complete ──────────┤
  │                                │  (detach_worktree, keep branch)
  │                                │◄── task B complete ──────────┤
  │                                │                              │
  ├─ get_plan_status ─────────────►│                              │
  │◄─ "awaiting_review", 2 tasks ─┤                              │
  │                                │                              │
  ├─ get_task_diff(A) ────────────►│                              │
  │◄─ full diff + summary ────────┤                              │
  │  (brain reviews code)          │                              │
  ├─ review_task(A, approve) ─────►│                              │
  │◄─ counts: 1 approved, 1 left ─┤  (beads: A → done)           │
  │                                │                              │
  ├─ get_task_diff(B) ────────────►│                              │
  │◄─ full diff + summary ────────┤                              │
  │  (brain reviews code)          │                              │
  ├─ review_task(B, approve) ─────►│                              │
  │◄─ ready_to_merge: true ────── ┤  (beads: B → done)           │
  │                                │                              │
  ├─ create_pr(branch) ──────────►│  (Phase 1: manual branch)    │
  │                                │  (Phase 3: merge_plan first) │
```

## What Is NOT In Phase 1

- `request_changes` decision (Phase 2)
- Re-dispatch with feedback context (Phase 2)
- Attempt tracking / iteration history (Phase 2)
- Review-gated dependency dispatch (Phase 2)
- `merge_plan` tool (Phase 3)
- Cherry-pick approved branches into feature branch (Phase 3)
- Worker branch cleanup (Phase 3, after merge)
- Plan state persistence to disk (future)
