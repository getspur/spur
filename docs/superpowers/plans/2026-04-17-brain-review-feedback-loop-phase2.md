# Brain-Driven Review Feedback Loop — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `request_changes` decision with worker re-dispatch + accumulated feedback, tighten dep-unblock to require `Approved`, and emit TUI events for plan reviews — completing the brain-driven review feedback loop.

**Architecture:** Extend `review_task` to handle three decisions (approve, reject, request_changes). Extend `PlanTaskEntry` with `attempt` + `history` fields. Share dispatch logic across initial kickoff (`run_plan`), approval cascade, and iteration by inlining the same try_send+spawn-completion block in `review_task`. Use trait injection (`McpEventSink` in `spur-mcp`, `impl` in `spur-core`) to emit events across the circular-dependency barrier.

**Tech Stack:** Rust, tokio (mpsc / oneshot / Mutex), tokio_util::TaskTracker, uuid, serde_json, spur MCP server (JSON-RPC).

**Spec:** `docs/superpowers/specs/2026-04-17-brain-review-feedback-loop-phase2-design.md`
**Depends on:** Phase 1 (shipped — see `docs/superpowers/specs/2026-04-17-brain-review-feedback-loop-design.md`)

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/spur-acp/src/domain/events.rs` | Modify | Add 2 new `SpurEventBody` variants |
| `crates/spur-mcp/src/events.rs` | **Create** | Define `McpEventSink` trait |
| `crates/spur-mcp/src/lib.rs` | Modify | Export new `events` module |
| `crates/spur-mcp/src/plan.rs` | Modify | `attempt` + `history` fields, `request_changes` branch, shared dispatch helper, rejection+approval cascades, enriched-task builder |
| `crates/spur-mcp/src/server.rs` | Modify | Add `event_sink` field, thread sink + `delegation_tx` + `task_tracker` to `review_task` |
| `crates/spur-mcp/src/tools.rs` | Modify | Update `review_task_def` to accept `request_changes` |
| `crates/spur-core/src/event_funnel.rs` | Modify | `impl spur_mcp::McpEventSink for FunnelHandle` |
| `crates/spur-core/src/orchestrator.rs` | Modify | Pass `Some(Arc::new(funnel.clone()))` to `McpCallbackServer::new` at 3 call sites |
| `crates/spur-tui/src/views/dashboard.rs` | Modify | Render 2 new event variants in activity log |

---

### Task 1: Add `SpurEventBody` variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (insert before closing `}` at ~line 472)

- [ ] **Step 1: Add the two new variants**

Find the `SpurEventBody` enum. The last variant today is `WorkerFileTouched` ending around line 471. Before the enum's closing `}`, add:

```rust
    /// Brain submitted a review verdict on a plan task.
    PlanTaskReviewed {
        plan_id: String,
        task_id: String,
        /// "approve" | "reject" | "request_changes"
        decision: String,
        feedback: Option<String>,
        attempt: u32,
    },

    /// A plan task was re-dispatched for iteration (attempt > 1).
    PlanTaskIterating {
        plan_id: String,
        task_id: String,
        /// New attempt number (the attempt that just started, i.e., old_attempt + 1).
        attempt: u32,
        delegation_id: String,
    },
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p spur-acp 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(acp): PlanTaskReviewed + PlanTaskIterating event variants"
```

---

### Task 2: Create `McpEventSink` trait module

**Files:**
- Create: `crates/spur-mcp/src/events.rs`
- Modify: `crates/spur-mcp/src/lib.rs`

- [ ] **Step 1: Create the trait module**

Write to `crates/spur-mcp/src/events.rs`:

```rust
//! Event sink trait used by the MCP callback server to emit plan-review
//! lifecycle events. A trait is used instead of a direct `FunnelHandle`
//! reference because `spur-core` depends on `spur-mcp` — adding a reverse
//! dependency would create a circular dependency.
//!
//! `spur-core` implements `McpEventSink for FunnelHandle` and injects it at
//! `McpCallbackServer` construction.

use spur_acp::SpurEventBody;

/// Emit plan-review lifecycle events to the process-wide event funnel.
pub trait McpEventSink: Send + Sync {
    fn emit(&self, event: SpurEventBody);
}
```

- [ ] **Step 2: Export the module from `lib.rs`**

Modify `crates/spur-mcp/src/lib.rs`. Current content:
```rust
pub mod plan;
pub mod server;
pub mod tools;

pub use server::{build_worker_info, McpCallbackServer, WorkerInfo};
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};
```

Change to:
```rust
pub mod events;
pub mod plan;
pub mod server;
pub mod tools;

pub use events::McpEventSink;
pub use server::{build_worker_info, McpCallbackServer, WorkerInfo};
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/events.rs crates/spur-mcp/src/lib.rs
git commit -m "feat(mcp): McpEventSink trait for cross-crate event emission"
```

---

### Task 3: Implement `McpEventSink` for `FunnelHandle` in spur-core

**Files:**
- Modify: `crates/spur-core/src/event_funnel.rs` (append after existing `impl FunnelHandle` block at line 33)

- [ ] **Step 1: Add the impl**

The file currently ends the `impl FunnelHandle { pub fn emit(...) }` block at line 33 then has the free function `spawn_funnel`. Insert this `impl` block between them (around line 34):

```rust
impl spur_mcp::McpEventSink for FunnelHandle {
    fn emit(&self, event: SpurEventBody) {
        // Delegates to the inherent `FunnelHandle::emit` method defined above.
        FunnelHandle::emit(self, event);
    }
}
```

If `spur_mcp` is not yet imported at the top of the file, add to imports:
```rust
use spur_mcp::McpEventSink;
```
then write `impl McpEventSink for FunnelHandle` (no prefix). Check which the file uses; prefer the fully-qualified `spur_mcp::McpEventSink` if there's no existing `use spur_mcp` import.

- [ ] **Step 2: Add `spur-mcp` to `spur-core` dependency (should already be present)**

Verify `crates/spur-core/Cargo.toml` line 11 has `spur-mcp = { workspace = true }`. If present (it already is), no action needed.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-core 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/event_funnel.rs
git commit -m "feat(core): impl McpEventSink for FunnelHandle — bridge to MCP"
```

---

### Task 4: Extend `PlanTaskEntry` with attempt + history fields

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (struct at lines 67-74 + all construction sites)

- [ ] **Step 1: Add `AttemptRecord` struct and extend `PlanTaskEntry`**

Find `PlanTaskEntry` (around line 67). Replace:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    pub result: Option<DelegationResult>,
    pub worker_branch: Option<String>,
}
```

with:

```rust
/// Record of a single attempt at a plan task. Stored in `PlanTaskEntry.history`
/// for attempts 1..attempt-1. The current (latest) attempt lives in the entry's
/// top-level `result` and `worker_branch` fields.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub worker_branch: Option<String>,
    pub diff_summary: Option<spur_acp::DiffSummary>,
    pub summary: Option<String>,
    /// Brain's `request_changes` feedback that caused this attempt to be superseded.
    pub feedback: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    /// Latest attempt's full delegation result. None while Dispatched.
    pub result: Option<DelegationResult>,
    /// Latest attempt's worker branch (preserved in git when set).
    pub worker_branch: Option<String>,
    /// Current attempt number — starts at 1 on initial dispatch.
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// Prior attempts (1..attempt-1). Empty for first-iteration tasks.
    #[serde(default)]
    pub history: Vec<AttemptRecord>,
}

fn default_attempt() -> u32 {
    1
}
```

- [ ] **Step 2: Fix all `PlanTaskEntry { ... }` construction sites**

Search: `grep -n "PlanTaskEntry {" crates/spur-mcp/src/plan.rs crates/spur-mcp/src/server.rs`

Each construction needs `attempt: 1` and `history: Vec::new()` added. For example (in plan.rs around line 124, inside `validate_plan`):

```rust
let entry = PlanTaskEntry {
    spec: task.clone(),
    status: PlanTaskStatus::Pending,
    result: None,
    worker_branch: None,
    attempt: 1,            // NEW
    history: Vec::new(),   // NEW
};
```

Apply the same two new fields to every construction site.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -5`
Expected: `Finished` with 0 errors. If errors appear, they will be at any construction sites missing the two new fields — add them there too.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/src/server.rs
git commit -m "feat(plan): attempt + history fields + AttemptRecord struct"
```

---

### Task 5: Constants and helper — `MAX_ATTEMPTS` and enriched-task builder

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (add near top of file after imports)

- [ ] **Step 1: Add constant and helper function**

After the `use` imports (around line 18, before the `PlanTask` struct), add:

```rust
/// Maximum number of iterations per plan task. After this many attempts,
/// `review_task(request_changes)` returns an error — the brain must approve,
/// reject, or leave the task as-is.
pub const MAX_ATTEMPTS: u32 = 3;

/// Build the enriched task description used when re-dispatching a task for
/// iteration. Concatenates the original task, prior attempt summaries, and
/// the brain's feedback. No bloat cap — the 3-attempt limit bounds size.
fn build_enriched_task(
    original_task: &str,
    history: &[AttemptRecord],
    current_feedback: &str,
) -> String {
    let mut out = String::with_capacity(
        original_task.len() + current_feedback.len() + history.len() * 512,
    );
    out.push_str("## Original Task\n");
    out.push_str(original_task);
    out.push_str("\n\n## Previous Attempts\n");
    for rec in history {
        out.push_str(&format!(
            "\nAttempt {attempt} (branch {branch}):\n  Summary: {summary}\n  Diff: {diff}\n  Brain feedback: {feedback}\n",
            attempt = rec.attempt,
            branch = rec.worker_branch.as_deref().unwrap_or("—"),
            summary = rec.summary.as_deref().unwrap_or("—"),
            diff = rec
                .diff_summary
                .as_ref()
                .map(|d| format!("+{}/-{} across {} files", d.insertions, d.deletions, d.files_changed))
                .unwrap_or_else(|| "—".to_string()),
            feedback = rec.feedback,
        ));
    }
    out.push_str("\n## Current Request\n");
    out.push_str(current_feedback);
    out.push_str(
        "\n\nApply the feedback above. You can inspect prior attempts with `git show <branch>` if helpful.\n",
    );
    out
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` (may show an "unused function" warning — acceptable; Task 7 will call it).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): MAX_ATTEMPTS const + build_enriched_task helper"
```

---

### Task 6: Tighten dependency-unblock to require `Approved` only (Subsystem B)

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` — the `Pending → Ready` check inside `run_plan`

- [ ] **Step 1: Find the dep-readiness check in `run_plan`**

Search: `grep -n "AwaitingReview" crates/spur-mcp/src/plan.rs`

Phase 1 already treats `AwaitingReview | Approved` as "done enough" in the readiness check. Locate lines around 182-185 (inside `run_plan`'s dispatch loop):

```rust
.filter(|t| matches!(
    t.status,
    PlanTaskStatus::AwaitingReview { .. }
        | PlanTaskStatus::Approved { .. }
))
```

- [ ] **Step 2: Change to `Approved` only**

Replace with:

```rust
.filter(|t| matches!(
    t.status,
    PlanTaskStatus::Approved { .. }
))
```

Also search for the same pattern in `build_plan_status` (around lines 435-441) where `blocked_by` is computed for Pending tasks. Apply the same tightening:

```rust
.filter(|d| {
    !state.tasks.iter().any(|o| {
        o.spec.task_id == **d
            && matches!(o.status, PlanTaskStatus::Approved { .. })
    })
})
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): tighten dep-unblock to Approved-only (Subsystem B)"
```

---

### Task 7: Rewrite `review_task` with request_changes + cascade logic

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (replace current `review_task` at lines 530-615)

- [ ] **Step 1: Update `review_task` signature**

Replace the current signature:
```rust
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
) -> Result<serde_json::Value, String>
```

with:
```rust
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
    sink: Option<&dyn crate::events::McpEventSink>,
    delegation_tx: Option<&tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>>,
    task_tracker: Option<&tokio_util::task::TaskTracker>,
    plan_arc: Option<std::sync::Arc<tokio::sync::Mutex<PlanState>>>,
) -> Result<serde_json::Value, String>
```

The extra `plan_arc` is needed because completion futures spawned into `task_tracker` must `.await` a lock on the plan AFTER `review_task` returns (the caller's `state: &mut` borrow is released).

- [ ] **Step 2: Replace the body**

Replace the full body of `review_task` with the following. The structure: validate task, match on decision, call a local `dispatch_for_task` helper for approve-cascade + request_changes paths.

```rust
    use tokio::sync::oneshot;
    use uuid::Uuid;
    let mut warnings: Vec<String> = Vec::new();

    // Validate the task exists and is in AwaitingReview.
    let (summary, _current_attempt) = {
        let entry = state
            .tasks
            .iter()
            .find(|t| t.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;
        match &entry.status {
            PlanTaskStatus::AwaitingReview { summary } => (summary.clone(), entry.attempt),
            other => {
                let name = match other {
                    PlanTaskStatus::Pending => "pending",
                    PlanTaskStatus::Ready => "ready",
                    PlanTaskStatus::Dispatched { .. } => "dispatched",
                    PlanTaskStatus::Approved { .. } => "approved",
                    PlanTaskStatus::Rejected { .. } => "rejected",
                    PlanTaskStatus::Failed { .. } => "failed",
                    _ => "unknown",
                };
                return Err(format!(
                    "task '{task_id}' is not awaiting review (current status: {name})"
                ));
            }
        }
    };

    // --- Dispatch branch per decision ---
    let mut new_dispatches: Vec<(String, u32, String)> = Vec::new(); // (task_id, attempt, delegation_id)

    match decision {
        "approve" => {
            // Mark Approved.
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            entry.status = PlanTaskStatus::Approved { summary: summary.clone() };
            let issue_id = entry.spec.issue_id.clone();

            // Beads sync (non-blocking).
            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some("done".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }

            // Approval cascade: dispatch any Pending tasks whose deps are now all Approved.
            if let (Some(tx), Some(tracker), Some(arc)) =
                (delegation_tx, task_tracker, plan_arc.clone())
            {
                dispatch_newly_ready(
                    plan_id,
                    state,
                    tx,
                    tracker,
                    arc,
                    sink,
                    &mut warnings,
                    &mut new_dispatches,
                );
            }
        }
        "reject" => {
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            entry.status = PlanTaskStatus::Rejected {
                feedback: feedback.map(String::from),
            };
            let issue_id = entry.spec.issue_id.clone();

            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain rejected: {}",
                        feedback.unwrap_or("does not meet requirements")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some("open".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }

            // Rejection cascade: mark all transitively-dependent tasks as Failed.
            mark_descendants_failed(task_id, state, &mut warnings);
        }
        "request_changes" => {
            let fb = feedback.ok_or_else(|| {
                "request_changes requires feedback".to_string()
            })?;

            // Validate attempt < MAX_ATTEMPTS.
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            if entry.attempt >= MAX_ATTEMPTS {
                return Err(format!(
                    "task is at max attempts ({MAX_ATTEMPTS}); approve, reject, or leave as-is"
                ));
            }

            let (tx, tracker, arc) = match (delegation_tx, task_tracker, plan_arc.clone()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => {
                    return Err(
                        "request_changes requires orchestrator channel (internal error)"
                            .to_string(),
                    );
                }
            };

            // Build enriched task BEFORE mutating state (reads history + spec).
            let enriched = build_enriched_task(&entry.spec.task, &entry.history, fb);

            let new_attempt = entry.attempt + 1;
            let delegation_id = Uuid::new_v4().to_string();
            let (resp_tx, resp_rx) = oneshot::channel::<DelegationResult>();

            let req = crate::tools::DelegationRequest {
                from: state.brain_session_id.clone(),
                to_agent: entry.spec.agent.clone(),
                task: enriched,
                delegation_plan: None,
                issue_id: entry.spec.issue_id.clone(),
                context_files: entry.spec.context_files.clone(),
                respond_to: resp_tx,
                request_id: delegation_id.clone(),
            };

            // try_send — atomic. Fail fast if channel full/closed.
            if let Err(e) = tx.try_send(req) {
                return Err(format!("orchestrator channel error: {e}"));
            }

            // Mutate state AFTER successful send.
            let prev_record = AttemptRecord {
                attempt: entry.attempt,
                worker_branch: entry.worker_branch.take(),
                diff_summary: entry
                    .result
                    .as_ref()
                    .and_then(|r| r.diff_summary.clone()),
                summary: entry
                    .result
                    .as_ref()
                    .and_then(|r| r.summary.clone()),
                feedback: fb.to_string(),
            };
            entry.history.push(prev_record);
            entry.result = None;
            entry.attempt = new_attempt;
            entry.status = PlanTaskStatus::Dispatched {
                delegation_id: delegation_id.clone(),
            };

            // Spawn completion future.
            spawn_completion_future(
                task_id.to_string(),
                delegation_id.clone(),
                resp_rx,
                arc,
                tracker,
            );

            new_dispatches.push((task_id.to_string(), new_attempt, delegation_id));
        }
        other => {
            return Err(format!(
                "invalid decision '{other}': must be 'approve', 'reject', or 'request_changes'"
            ));
        }
    }

    // Build response (uses updated state).
    let mut resp = build_plan_status(plan_id, state);
    if let serde_json::Value::Object(ref mut m) = resp {
        m.insert("task_id".into(), serde_json::json!(task_id));
        m.insert("decision".into(), serde_json::json!(decision));
        m.insert("warnings".into(), serde_json::json!(warnings));
        if decision == "request_changes" {
            if let Some((_, new_att, did)) = new_dispatches.iter().find(|(tid, _, _)| tid == task_id) {
                m.insert("new_attempt".into(), serde_json::json!(new_att));
                m.insert("new_delegation_id".into(), serde_json::json!(did));
            }
        }
    }

    // --- Emit events ---
    if let Some(sink) = sink {
        sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
            plan_id: plan_id.to_string(),
            task_id: task_id.to_string(),
            decision: decision.to_string(),
            feedback: feedback.map(String::from),
            attempt: _current_attempt,
        });
        for (tid, attempt, did) in &new_dispatches {
            if tid == task_id && decision == "request_changes" {
                sink.emit(spur_acp::SpurEventBody::PlanTaskIterating {
                    plan_id: plan_id.to_string(),
                    task_id: tid.clone(),
                    attempt: *attempt,
                    delegation_id: did.clone(),
                });
            }
        }
    }

    Ok(resp)
}
```

- [ ] **Step 3: Add the three helper functions below `review_task`**

Append these helpers inside `plan.rs` (after `review_task`):

```rust
/// BFS from the rejected task through the dependency graph; mark each
/// transitively-dependent task as Failed. Called on reject decisions.
fn mark_descendants_failed(
    rejected_task_id: &str,
    state: &mut PlanState,
    warnings: &mut Vec<String>,
) {
    use std::collections::VecDeque;
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(rejected_task_id.to_string());
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(rejected_task_id.to_string());

    while let Some(parent) = queue.pop_front() {
        // Find all tasks that depend on `parent`.
        let dependents: Vec<String> = state
            .tasks
            .iter()
            .filter(|t| t.spec.depends_on.iter().any(|d| d == &parent))
            .map(|t| t.spec.task_id.clone())
            .collect();

        for dep_id in dependents {
            if !visited.insert(dep_id.clone()) {
                continue;
            }
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == dep_id)
                .unwrap();
            // Only cascade through tasks that haven't already reached a terminal state.
            let should_fail = matches!(
                entry.status,
                PlanTaskStatus::Pending | PlanTaskStatus::Ready
            );
            if should_fail {
                entry.status = PlanTaskStatus::Failed {
                    error: format!("upstream '{parent}' rejected"),
                };
                queue.push_back(dep_id);
            } else {
                warnings.push(format!(
                    "descendant '{dep_id}' not cascaded (already in terminal state)"
                ));
            }
        }
    }
}

/// Scan for Pending tasks whose deps are all Approved; dispatch each.
fn dispatch_newly_ready(
    plan_id: &str,
    state: &mut PlanState,
    delegation_tx: &tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    task_tracker: &tokio_util::task::TaskTracker,
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    sink: Option<&dyn crate::events::McpEventSink>,
    warnings: &mut Vec<String>,
    new_dispatches: &mut Vec<(String, u32, String)>,
) {
    use tokio::sync::oneshot;
    use uuid::Uuid;

    let ready_ids: Vec<String> = state
        .tasks
        .iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::Pending))
        .filter(|t| {
            t.spec.depends_on.iter().all(|dep| {
                state.tasks.iter().any(|o| {
                    o.spec.task_id == *dep
                        && matches!(o.status, PlanTaskStatus::Approved { .. })
                })
            })
        })
        .map(|t| t.spec.task_id.clone())
        .collect();

    for task_id in ready_ids {
        let (agent, task, issue_id, context_files) = {
            let e = state.tasks.iter().find(|t| t.spec.task_id == task_id).unwrap();
            (
                e.spec.agent.clone(),
                e.spec.task.clone(),
                e.spec.issue_id.clone(),
                e.spec.context_files.clone(),
            )
        };
        let delegation_id = Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = oneshot::channel::<DelegationResult>();

        let req = crate::tools::DelegationRequest {
            from: state.brain_session_id.clone(),
            to_agent: agent,
            task,
            delegation_plan: None,
            issue_id,
            context_files,
            respond_to: resp_tx,
            request_id: delegation_id.clone(),
        };

        match delegation_tx.try_send(req) {
            Ok(()) => {
                let entry = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id).unwrap();
                entry.status = PlanTaskStatus::Dispatched {
                    delegation_id: delegation_id.clone(),
                };
                spawn_completion_future(
                    task_id.clone(),
                    delegation_id.clone(),
                    resp_rx,
                    plan_arc.clone(),
                    task_tracker,
                );
                new_dispatches.push((task_id.clone(), 1, delegation_id));
            }
            Err(e) => {
                let entry = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id).unwrap();
                entry.status = PlanTaskStatus::Failed {
                    error: format!("failed to dispatch: {e}"),
                };
                warnings.push(format!("dispatch failed for '{task_id}': {e}"));
            }
        }
    }
    let _ = (plan_id, sink); // reserved for future event emission on cascade dispatch
}

/// Spawn a future that awaits a DelegationResult and writes it back to PlanState.
/// Guards against stale completions (if the task was iterated again before this
/// resolved, the delegation_id no longer matches — result is discarded).
fn spawn_completion_future(
    task_id: String,
    expected_delegation_id: String,
    rx: tokio::sync::oneshot::Receiver<DelegationResult>,
    plan_arc: std::sync::Arc<tokio::sync::Mutex<PlanState>>,
    task_tracker: &tokio_util::task::TaskTracker,
) {
    task_tracker.spawn(async move {
        let Ok(result) = rx.await else {
            // Oneshot sender dropped — orchestrator died or task cancelled.
            let mut state = plan_arc.lock().await;
            if let Some(entry) = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id) {
                if let PlanTaskStatus::Dispatched { ref delegation_id } = entry.status {
                    if delegation_id == &expected_delegation_id {
                        entry.status = PlanTaskStatus::Failed {
                            error: "orchestrator channel dropped".to_string(),
                        };
                    }
                }
            }
            return;
        };

        let mut state = plan_arc.lock().await;
        let Some(entry) = state.tasks.iter_mut().find(|t| t.spec.task_id == task_id) else {
            return;
        };

        // Stale-completion guard: only apply if we're still the expected attempt.
        let still_ours = matches!(
            &entry.status,
            PlanTaskStatus::Dispatched { delegation_id } if delegation_id == &expected_delegation_id
        );
        if !still_ours {
            return;
        }

        match &result.status {
            DelegationStatus::Success | DelegationStatus::Modified { .. } => {
                entry.status = PlanTaskStatus::AwaitingReview {
                    summary: result.summary.clone(),
                };
                entry.worker_branch = result.worker_branch.clone();
            }
            DelegationStatus::Failed { error } => {
                entry.status = PlanTaskStatus::Failed { error: error.clone() };
            }
            other => {
                entry.status = PlanTaskStatus::Failed {
                    error: format!("{other:?}"),
                };
            }
        }
        entry.result = Some(result);
    });
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -5`
Expected: `Finished` with 0 errors. (The old Phase 1 `review_task` callers in server.rs will now fail — fixed in Task 8.)

If there are compile errors in Step 4 related to `review_task` callers, that's expected. Move to Task 8 to fix them, then re-build.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): request_changes + cascades + spawn_completion_future"
```

---

### Task 8: Update `McpCallbackServer` to hold `event_sink` and thread through to `review_task`

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (struct at lines 139-158, constructor at line 165, `handle_review_task` at lines 1362-1379)

- [ ] **Step 1: Add `event_sink` field**

In `McpCallbackServer` struct definition (lines 139-158), add a new field (alongside `pm_service`):

```rust
    pm_service: Option<Arc<PmService>>,
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,  // NEW
    active_plans: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::plan::PlanState>>>>>,
```

- [ ] **Step 2: Update `McpCallbackServer::new` signature**

At line 165, change:
```rust
pub fn new(session_id: &SessionId, pm_service: Option<Arc<PmService>>) -> (Self, DelegationChannel)
```

to:
```rust
pub fn new(
    session_id: &SessionId,
    pm_service: Option<Arc<PmService>>,
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
) -> (Self, DelegationChannel)
```

Inside the body, where `Self { ... }` is constructed, add `event_sink,` alongside `pm_service,`.

- [ ] **Step 3: Rewrite `handle_review_task`**

Replace the current body (lines 1362-1379) with:

```rust
    async fn handle_review_task(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"].as_str().ok_or("missing plan_id")?.to_string();
        let task_id = args["task_id"].as_str().ok_or("missing task_id")?.to_string();
        let decision = args["decision"].as_str().ok_or("missing decision")?;
        let feedback = args["feedback"].as_str();

        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans.get(&plan_id).cloned()
                .ok_or_else(|| format!("unknown plan '{plan_id}'"))?
        };

        let pm = self.pm_service.as_deref();
        let sink: Option<&dyn crate::events::McpEventSink> = self.event_sink.as_deref();

        let mut state = plan_arc.lock().await;
        let result = crate::plan::review_task(
            &plan_id,
            &task_id,
            decision,
            feedback,
            &mut state,
            pm,
            sink,
            Some(&self.delegation_tx),
            Some(&self.task_tracker),
            Some(plan_arc.clone()),
        )
        .await?;
        drop(state);

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }
```

Note the `drop(state)` — required because `task_tracker.spawn` inside `review_task` wants its own lock later, and we're explicit about releasing.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -5`
Expected: `Finished` — or errors only at `McpCallbackServer::new` call sites (fixed in Task 9).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(mcp): McpCallbackServer holds event_sink, threaded to review_task"
```

---

### Task 9: Pass event sink at `McpCallbackServer::new` construction sites

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — three call sites: lines ~520, ~1498, ~1649

- [ ] **Step 1: Find all three call sites**

Run: `grep -n "McpCallbackServer::new" crates/spur-core/src/orchestrator.rs`

- [ ] **Step 2: Update each call site**

For each call site, the current code is:
```rust
let (mcp_server, delegation_channel) =
    McpCallbackServer::new(&session_id, self.pm_service.clone());
```

The variable holding the `FunnelHandle` at each site is named `funnel` (or similar — check locally). Change the call to:

```rust
let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
    Some(std::sync::Arc::new(funnel.clone()));
let (mcp_server, delegation_channel) =
    McpCallbackServer::new(&session_id, self.pm_service.clone(), sink);
```

Do this at all three locations. If a call site doesn't have `funnel` in scope, pass `None` for now — the existing tests that hit those paths still work with `None`. (But for production paths 520, 1498, 1649, `funnel` should be in scope since the orchestrator holds one.)

- [ ] **Step 3: Verify the full workspace builds**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(core): pass McpEventSink to MCP callback server"
```

---

### Task 10: Update `review_task_def` in tools.rs

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (function `review_task_def` at lines 695-727)

- [ ] **Step 1: Update the description and decision enum**

Replace the body of `review_task_def()` with:

```rust
pub fn review_task_def() -> ToolDefinition {
    ToolDefinition {
        name: "review_task".to_string(),
        description: "Submit a review decision for a plan task awaiting review. \
            Three decisions: 'approve' (task done, beads→done), 'reject' (task \
            dead, beads→open, dependent tasks auto-failed), or 'request_changes' \
            (re-dispatch worker with feedback — max 3 attempts per task, requires \
            `feedback`). After approve, dependent tasks whose deps are now all \
            Approved are auto-dispatched. Returns updated plan status with counts, \
            ready_to_merge flag, and (for request_changes) new_attempt + \
            new_delegation_id fields."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task_id to review"
                },
                "decision": {
                    "type": "string",
                    "enum": ["approve", "reject", "request_changes"],
                    "description": "Review verdict"
                },
                "feedback": {
                    "type": "string",
                    "description": "Review notes. Required for request_changes, optional for approve/reject."
                }
            },
            "required": ["plan_id", "task_id", "decision"]
        }),
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "feat(mcp): review_task accepts request_changes decision"
```

---

### Task 10b: Extend `get_task_diff` with optional `attempt` parameter

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` — `get_task_diff_def()` function
- Modify: `crates/spur-mcp/src/server.rs` — `handle_get_task_diff` function

- [ ] **Step 1: Add `attempt` to the input schema**

In `get_task_diff_def()`, update `input_schema`:

```rust
input_schema: json!({
    "type": "object",
    "properties": {
        "plan_id": { "type": "string", "description": "The plan_id returned by submit_plan" },
        "task_id": { "type": "string", "description": "The task_id to inspect" },
        "attempt": {
            "type": "integer",
            "description": "Optional: inspect a prior attempt (1..current-1). Omit for the latest attempt."
        }
    },
    "required": ["plan_id", "task_id"]
}),
```

Also update the description to mention the new param:
```rust
description: "Get the full unified diff for a plan task. Use after \
    get_plan_status shows tasks in awaiting_review, approved, rejected, or \
    failed state. Returns the complete diff, worker branch name, task \
    description, and summary for brain code review. Pass `attempt` to inspect \
    prior iteration attempts (see entry.history)."
    .to_string(),
```

- [ ] **Step 2: Update `handle_get_task_diff` in server.rs to read `attempt`**

Find `handle_get_task_diff` (around server.rs line 1303+). After parsing `plan_id` and `task_id`, also parse `attempt`:

```rust
let attempt = args["attempt"].as_u64().map(|n| n as u32);
```

After locating the entry, if `attempt` is `Some(n)` and `n != entry.attempt`, look up the prior attempt in `entry.history`:

```rust
// After successful entry lookup, before building the response:
if let Some(want_attempt) = attempt {
    if want_attempt == entry.attempt {
        // caller asked for current; proceed as today
    } else {
        // Look up in history.
        let Some(rec) = entry.history.iter().find(|r| r.attempt == want_attempt) else {
            return Err(format!(
                "task '{task_id}' has no attempt {want_attempt} (current: {}, history: {} entries)",
                entry.attempt,
                entry.history.len()
            ));
        };
        // Build response from AttemptRecord (no full diff — we only stored summaries).
        let mut resp = serde_json::Map::new();
        resp.insert("task_id".into(), json!(task_id));
        resp.insert("agent".into(), json!(entry.spec.agent));
        resp.insert("attempt".into(), json!(want_attempt));
        resp.insert("status".into(), json!("historical"));
        resp.insert("task_description".into(), json!(entry.spec.task));
        if let Some(ref id) = entry.spec.issue_id {
            resp.insert("issue_id".into(), json!(id));
        }
        if let Some(ref b) = rec.worker_branch {
            resp.insert("worker_branch".into(), json!(b));
        }
        if let Some(ref s) = rec.summary {
            resp.insert("summary".into(), json!(s));
        }
        if let Some(ref d) = rec.diff_summary {
            resp.insert("diff_summary".into(), serde_json::to_value(d).unwrap_or_default());
        }
        resp.insert("feedback".into(), json!(rec.feedback));
        resp.insert(
            "note".into(),
            json!("Historical attempt — full diff text not stored. Inspect git: `git show <worker_branch>`."),
        );
        return serde_json::to_string_pretty(&serde_json::Value::Object(resp))
            .map_err(|e| e.to_string());
    }
}
```

Place this block AFTER entry is located and before the current response-building code. The current-attempt path remains unchanged.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs
git commit -m "feat(mcp): get_task_diff accepts optional attempt param for history"
```

---

### Task 11: Render new events in the TUI activity log

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs` — `handle_spur_event` match at lines 935-1367

- [ ] **Step 1: Add two new match arms**

Locate the `match &event.body {` block starting around line 936 and the `_ => {}` catchall at line 1366. Just before the catchall, add:

```rust
            SpurEventBody::PlanTaskReviewed {
                plan_id: _,
                task_id,
                decision,
                feedback,
                attempt,
            } => {
                let (icon, color) = match decision.as_str() {
                    "approve" => ("✓", ratatui::style::Color::Green),
                    "reject" => ("✗", ratatui::style::Color::Red),
                    "request_changes" => ("↻", ratatui::style::Color::Yellow),
                    _ => ("?", ratatui::style::Color::Gray),
                };
                let fb_suffix = feedback
                    .as_ref()
                    .map(|f| format!(": \"{f}\""))
                    .unwrap_or_default();
                let text = format!(
                    "{icon} Brain {decision} task {task_id} (attempt {attempt}){fb_suffix}"
                );
                self.activity_log.push(ActivityLogEntry::new(
                    event.timestamp,
                    text,
                    color,
                ));
            }
            SpurEventBody::PlanTaskIterating {
                plan_id: _,
                task_id,
                attempt,
                delegation_id: _,
            } => {
                let text = format!("↻ Task {task_id} iterating (attempt {attempt})");
                self.activity_log.push(ActivityLogEntry::new(
                    event.timestamp,
                    text,
                    ratatui::style::Color::Cyan,
                ));
            }
```

If the codebase's activity log API differs (e.g., uses a different push method or different style type), follow the existing pattern visible at nearby arms like `DelegationCompleted` (around line 1019) and mirror its shape.

- [ ] **Step 2: Verify the TUI crate compiles**

Run: `cargo build -p spur-tui 2>&1 | tail -5`
Expected: `Finished` with 0 errors. (If the activity log signature differs, fix to match.)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(tui): render PlanTaskReviewed + PlanTaskIterating events"
```

---

### Task 12: Add unit tests for new behaviors

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` — test module at lines 619-694

- [ ] **Step 1: Add test for enriched-task builder**

At the bottom of the `#[cfg(test)] mod tests { ... }` block, before the closing `}` of the module, add:

```rust
    #[test]
    fn enriched_task_includes_original_history_and_feedback() {
        let history = vec![
            AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-x".to_string()),
                diff_summary: None,
                summary: Some("did thing".to_string()),
                feedback: "add null check".to_string(),
            },
        ];
        let enriched = super::build_enriched_task(
            "Implement foo",
            &history,
            "now also handle empty input",
        );
        assert!(enriched.contains("Implement foo"));
        assert!(enriched.contains("Attempt 1"));
        assert!(enriched.contains("add null check"));
        assert!(enriched.contains("now also handle empty input"));
        assert!(enriched.contains("git show"));
    }

    #[test]
    fn enriched_task_empty_history_still_well_formed() {
        let enriched = super::build_enriched_task("Task X", &[], "fb");
        assert!(enriched.contains("Task X"));
        assert!(enriched.contains("fb"));
        // Previous Attempts section present but empty.
        assert!(enriched.contains("## Previous Attempts"));
    }

    #[test]
    fn max_attempts_is_three() {
        assert_eq!(super::MAX_ATTEMPTS, 3);
    }
```

- [ ] **Step 2: Add test for rejection cascade helper**

Inside the same test module, add:

```rust
    #[test]
    fn rejection_cascade_marks_descendants_failed() {
        use spur_acp::SessionId;
        let tasks = vec![
            super::PlanTask {
                task_id: "A".to_string(),
                agent: "x".to_string(),
                task: "a".to_string(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "B".to_string(),
                agent: "x".to_string(),
                task: "b".to_string(),
                depends_on: vec!["A".to_string()],
                issue_id: None,
                context_files: vec![],
            },
            super::PlanTask {
                task_id: "C".to_string(),
                agent: "x".to_string(),
                task: "c".to_string(),
                depends_on: vec!["B".to_string()],
                issue_id: None,
                context_files: vec![],
            },
        ];
        let mut state = super::PlanState {
            plan_id: "p".to_string(),
            tasks: tasks
                .into_iter()
                .map(|t| super::PlanTaskEntry {
                    spec: t,
                    status: super::PlanTaskStatus::Pending,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                })
                .collect(),
            brain_session_id: SessionId("brain".to_string()),
        };
        let mut warnings = Vec::new();
        super::mark_descendants_failed("A", &mut state, &mut warnings);

        // B and C should now be Failed; A remains Pending (caller sets it separately).
        let b = state.tasks.iter().find(|t| t.spec.task_id == "B").unwrap();
        let c = state.tasks.iter().find(|t| t.spec.task_id == "C").unwrap();
        assert!(matches!(b.status, super::PlanTaskStatus::Failed { .. }));
        assert!(matches!(c.status, super::PlanTaskStatus::Failed { .. }));
    }
```

- [ ] **Step 3: Run the full test suite**

Run: `cargo test -p spur-mcp 2>&1 | tail -10`
Expected: All existing tests pass (8 from Phase 1) + 4 new tests = 12 pass, 0 fail.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "test(plan): enriched task builder + rejection cascade tests"
```

---

### Task 13: Full workspace verification

**Files:** None (verification only)

- [ ] **Step 1: Full build**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 errors, 0 warnings.

- [ ] **Step 2: Full test run**

Run: `cargo test 2>&1 | grep "test result:" | sort -u`
Expected: All test suites show `ok` with 0 failures.

- [ ] **Step 3: Confirm tool count**

The MCP tool list should still have 24 tools (Phase 1 already added `get_task_diff` and `review_task`; Phase 2 only MODIFIES `review_task_def`, doesn't add new tools).

Run: `grep -c "_def()" crates/spur-mcp/src/tools.rs | head -1`

No new tools added in Phase 2 — we extended an existing one.

- [ ] **Step 4: Final commit if any fixups**

```bash
git status
# If clean, nothing to commit.
# If fixups needed:
git add -A && git commit -m "chore: fixups from integration verification"
```
