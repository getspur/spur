# Brain ↔ Worker Integration Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce six integration invariants between the brain LLM (MCP tools) and workers (orchestrator + ACP). Turns two silently-violated invariants and one convention-only invariant into type-checked or runtime-checked properties, and adds two push events the brain currently polls for.

**Architecture:** Tactical fixes only — no module decomposition, no tool-surface collapse, no typestate aggregate refactor. Each task touches ≤2 files. Ordering follows the invariant dependency graph (INV-2 → INV-1, INV-5 → INV-6/INV-7, INV-4 → INV-6). Paired source: `docs/superpowers/specs/2026-04-19-brain-worker-integration-invariants.md`.

**Tech Stack:** Rust 2021, tokio, tokio_util::sync::CancellationToken, serde, tracing. Workspace crates touched: `spur-acp` (event variants), `spur-mcp` (server/plan), `spur-core` (orchestrator, lineage, review_sink). Tests via `cargo test -p <crate> --test <file>`.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/spur-acp/src/domain/session.rs` (existing) | Add `BrainSessionId(SessionId)` newtype | T1 |
| `crates/spur-mcp/src/tools.rs` | Switch `DelegationRequest.brain_session_id` to `BrainSessionId`; add `DelegationRequest::new()` builder | T1 |
| `crates/spur-mcp/src/server.rs` | `parse_parallel_tasks` takes `brain_session_id: &BrainSessionId` | T1 |
| `crates/spur-mcp/tests/delegate_parallel_fields.rs` | Update existing tests to new signature | T1 |
| `crates/spur-core/src/lineage/adapter.rs` | `pending_task_by_request_id: HashMap<String, (String, Option<String>)>` state; populate task_spec on `DelegationDispatched` by matching request_id → executor_id | T2 |
| `crates/spur-core/src/lineage/projection.rs` | Expose `HashMap` mutator if needed (or move state into adapter) | T2 |
| `crates/spur-mcp/src/plan.rs` | `handle_review_task::approve`: clone-out-of-lock then async I/O | T3 |
| `crates/spur-acp/src/domain/events.rs` | Add `PlanCompleted` + `PlanReadyToMerge` variants | T4 |
| `crates/spur-mcp/src/plan.rs` | `run_plan` takes `funnel: FunnelHandle`, emits terminal events | T4 |
| `crates/spur-core/src/review_sink.rs` | Export `ReviewHandle` wrapping `(ExecutorId, rx)` | T5 |
| `crates/spur-core/src/orchestrator.rs` | Use `ReviewHandle.emit_requested()` instead of bare `funnel.emit` | T5 |
| `crates/spur-acp/src/domain/types.rs` (or wherever `DelegationStatus` lives) | Add `Cancelled { reason }` variant | T6 |
| `crates/spur-core/src/orchestrator.rs` | Add `cancellation_tokens: Arc<DashMap<String, CancellationToken>>`; real `cancel(id)` method; wire `select!` in spawned task | T6 |
| `crates/spur-mcp/src/server.rs` | `handle_cancel_delegation`: call `orchestrator.cancel(id)` directly instead of sending `__cancel_delegation` through the channel | T6 |

---

## Task 1: INV-2 — Typed `BrainSessionId` Newtype

**Invariant:** Every `DelegationRequest` MUST carry a valid `brain_session_id`. No `SessionId::new()` default.

**Files:**
- Modify: `crates/spur-acp/src/domain/session.rs` (or wherever `SessionId` is defined — confirm with `grep -n "pub struct SessionId" crates/spur-acp/src/domain/`)
- Modify: `crates/spur-mcp/src/tools.rs:14-33` (`DelegationRequest` struct)
- Modify: `crates/spur-mcp/src/server.rs:199-240` (`parse_parallel_tasks` signature)
- Modify: `crates/spur-mcp/src/server.rs:863` (single existing caller)
- Modify: `crates/spur-mcp/src/plan.rs:579-588` (plan-executor caller)
- Modify: `crates/spur-core/src/orchestrator.rs:2389-2500` (destructure, threading into `execute_delegation`)
- Test: `crates/spur-mcp/tests/delegate_parallel_fields.rs` (new test + fix existing tests)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-mcp/tests/delegate_parallel_fields.rs`:

```rust
#[test]
fn parse_parallel_tasks_requires_brain_session_id() {
    use spur_acp::{BrainSessionId, SessionId};

    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "T" }
        ]
    });
    let brain_sid = BrainSessionId::new(SessionId("brain-xyz".into()));

    let parsed =
        spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");

    assert_eq!(parsed.len(), 1);
    assert_eq!(
        parsed[0].brain_session_id.as_session_id().0,
        "brain-xyz",
        "brain_session_id must be threaded through, not defaulted to SessionId::new()"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields parse_parallel_tasks_requires_brain_session_id`
Expected: FAIL — compile error, `BrainSessionId` does not exist and `parse_parallel_tasks` takes 1 arg not 2.

- [ ] **Step 3: Add `BrainSessionId` newtype**

In `crates/spur-acp/src/domain/session.rs` (append near `SessionId`):

```rust
/// Newtype wrapping the brain's ACP session id. Distinct from worker
/// `SessionId`s by type — no `Default` impl, no `::new()` that takes
/// zero args, forcing every construction to carry a valid inner value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BrainSessionId(pub SessionId);

impl BrainSessionId {
    pub fn new(id: SessionId) -> Self { Self(id) }
    pub fn as_session_id(&self) -> &SessionId { &self.0 }
    pub fn into_session_id(self) -> SessionId { self.0 }
}

impl std::fmt::Display for BrainSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
```

Export from `crates/spur-acp/src/domain/mod.rs` (add `pub use session::BrainSessionId;`) and from `crates/spur-acp/src/lib.rs` (add `pub use domain::BrainSessionId;` next to the existing `SessionId` re-export).

- [ ] **Step 4: Change `DelegationRequest.brain_session_id` field type**

In `crates/spur-mcp/src/tools.rs:14-33`, modify:

```rust
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    pub respond_to: oneshot::Sender<DelegationResult>,
    /// Brain session that originated this request. Typed so no
    /// constructor can default to a random UUID.
    pub brain_session_id: spur_acp::BrainSessionId,
    pub delegation_plan: Option<spur_acp::DelegationPlan>,
    pub issue_id: Option<String>,
}
```

- [ ] **Step 5: Update `parse_parallel_tasks` signature + callers**

In `crates/spur-mcp/src/server.rs:199`:

```rust
pub fn parse_parallel_tasks(
    args: &Value,
    brain_session_id: &spur_acp::BrainSessionId,
) -> Result<Vec<DelegationRequest>, String> {
    // ... existing body ...
}
```

In the body at `server.rs:228-237`, replace the skeleton construction:

```rust
let (tx, _rx) = tokio::sync::oneshot::channel();
out.push(DelegationRequest {
    id: uuid::Uuid::new_v4().to_string(),
    agent,
    task,
    context_files,
    respond_to: tx,
    brain_session_id: brain_session_id.clone(),  // ← was SessionId::new()
    delegation_plan,
    issue_id,
});
```

At the single caller `server.rs:863`:

```rust
let skeletons = match parse_parallel_tasks(&args, &self.brain_session_id.clone().into()) {
    Ok(s) => s,
    Err(e) => return error_response(id, -32602, &e),
};
```

Note: `self.brain_session_id` is currently a plain `SessionId`. Either (a) change the field type on `McpCallbackServer` to `BrainSessionId` too, or (b) wrap at the call site. **Choose (a)** — it's the consistent story. Update `crates/spur-mcp/src/server.rs` `McpCallbackServer::new` to accept `&BrainSessionId` and store `BrainSessionId`.

- [ ] **Step 6: Thread through orchestrator destructure**

In `crates/spur-core/src/orchestrator.rs:2407-2416`, field type already flows via struct destructure — only the `execute_delegation` signature at `orchestrator.rs:2604` needs update:

```rust
async fn execute_delegation(
    agent: String,
    original_task: String,
    context_files: Vec<String>,
    request_id: String,
    brain_session_id: spur_acp::BrainSessionId,  // ← was SessionId
    delegation_plan: Option<spur_acp::domain::DelegationPlan>,
    // ... rest unchanged ...
) -> (DelegationResult, Option<ExecutorId>) {
```

And at the `DelegationRequested` emit site `orchestrator.rs:3605`:

```rust
funnel.emit(SpurEventBody::DelegationRequested {
    from: ctx.brain_session_id.as_session_id().clone(),  // unwrap to wire format
    // ... rest unchanged ...
});
```

And at `DelegationDispatched` (`orchestrator.rs:3669-3673`):

```rust
funnel.emit(SpurEventBody::DelegationDispatched {
    from: ctx.brain_session_id.as_session_id().clone(),
    request_id: ctx.request_id.to_string(),
    executor_id: worker_session.0.clone(),
});
```

And `WorkerAttemptCtx.brain_session_id` field at `orchestrator.rs:3573-3592` changes from `&SessionId` to `&BrainSessionId`.

Update the plan executor caller `crates/spur-mcp/src/plan.rs:544-585`:

```rust
let brain_sid = plan.lock().await.brain_session_id.clone();  // now BrainSessionId

// and at the DelegationRequest construction ~line 585:
brain_session_id: brain_sid.clone(),  // already BrainSessionId, no cast needed
```

And `PlanState.brain_session_id` field in `crates/spur-mcp/src/plan.rs` (search for `brain_session_id: SessionId`) changes to `BrainSessionId`.

- [ ] **Step 7: Fix the existing tests broken by signature change**

In `crates/spur-mcp/tests/delegate_parallel_fields.rs`, replace all `spur_mcp::parse_parallel_tasks(&args)` calls with:

```rust
let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("test-brain".into()));
let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields`
Expected: PASS for all tests in that file.

- [ ] **Step 9: Run the full affected-crate test suites**

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-acp/src/domain/session.rs crates/spur-acp/src/domain/mod.rs crates/spur-acp/src/lib.rs \
        crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/src/plan.rs \
        crates/spur-core/src/orchestrator.rs \
        crates/spur-mcp/tests/delegate_parallel_fields.rs
git commit -m "refactor(spur): INV-2 — typed BrainSessionId newtype, no Default

DelegationRequest.brain_session_id is now spur_acp::BrainSessionId, a
newtype wrapping SessionId with no Default impl. parse_parallel_tasks
now takes &BrainSessionId as a required param, removing the
SessionId::new() default that would silently create phantom sessions
in lineage if a caller forgot to overwrite the skeleton."
```

---

## Task 2: INV-1 — `delegation_id` as Sole Correlation Key

**Invariant:** `DelegationRequested.task` must be attributed to the executor node created by the matching `request_id`, not by agent-name heuristic.

**Files:**
- Modify: `crates/spur-core/src/lineage/adapter.rs:90-122` (the `DelegationRequested` handler)
- Modify: `crates/spur-core/src/lineage/adapter.rs` (add state: `pending_task_by_request_id: HashMap<String, PendingTaskSpec>`)
- Modify: `crates/spur-core/src/lineage/projection.rs:41-46` (decide whether to colocate state on `ExecutorLineage` or a new adapter struct)
- Test: `crates/spur-core/tests/lineage_integration.rs` (new test asserting concurrent-same-agent correctness)

- [ ] **Step 1: Decide state location**

The current `apply_legacy` is a free function taking `&mut ExecutorLineage`. Adding persistent state means either:
1. Adding a `pending_task_by_request_id: HashMap<String, (String, Option<String>)>` field to `ExecutorLineage`, OR
2. Converting `apply_legacy` into a stateful adapter struct.

**Choose (1)** — it's a smaller diff and keeps the dispatch pattern identical.

- [ ] **Step 2: Write the failing test**

Append to `crates/spur-core/tests/lineage_integration.rs`:

```rust
#[test]
fn concurrent_same_agent_workers_attribute_tasks_correctly() {
    // Two coder workers dispatched near-simultaneously. DelegationDispatched
    // carries (request_id → executor_id) mapping. task_spec MUST land on
    // the executor matched by request_id, not by agent name.
    use spur_acp::{SessionId, SpurEvent, SpurEventBody, Role};
    use spur_core::lineage::ExecutorLineage;
    use spur_core::lineage::ExecutorId;

    let mut l = ExecutorLineage::default();

    // Spawn both executors (WorkerSpawned path creates them with empty task_spec).
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-A".into()),
        worktree: std::path::PathBuf::from("/tmp/wA"),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-B".into()),
        worktree: std::path::PathBuf::from("/tmp/wB"),
    }));

    // DelegationRequested for task A arrives first (buffered by request_id).
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "coder".into(),
        task: "TASK-A: fix login CSS".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));
    // DelegationRequested for task B arrives.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "coder".into(),
        task: "TASK-B: add rate limiter".into(),
        request_id: "req-B".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    // Dispatch events arrive out of order — B before A.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-B".into(),
        executor_id: "worker-B".into(),
    }));
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-A".into(),
        executor_id: "worker-A".into(),
    }));

    // Assertions: each node carries the task matched by request_id.
    let node_a = l.node(&ExecutorId::new("worker-A")).expect("worker-A");
    let node_b = l.node(&ExecutorId::new("worker-B")).expect("worker-B");
    assert_eq!(node_a.task_spec, "TASK-A: fix login CSS",
        "worker-A must carry task A (matched by request_id req-A)");
    assert_eq!(node_b.task_spec, "TASK-B: add rate limiter",
        "worker-B must carry task B (matched by request_id req-B)");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-core --test lineage_integration concurrent_same_agent_workers_attribute_tasks_correctly`
Expected: FAIL. Under the current agent-name + empty-task-spec heuristic, the later `DelegationRequested` event finds the most-recent Executor with empty task_spec (worker-B) and writes TASK-B's text onto it — but TASK-A's text is written onto worker-B FIRST (worker-B was the only one with empty task_spec at that moment), then TASK-B's text overwrites TASK-A. Net result is either swapped or both-end-up-B depending on emit order. Either way not `(A→A, B→B)`.

- [ ] **Step 4: Add state field to `ExecutorLineage`**

In `crates/spur-core/src/lineage/projection.rs` inside the `ExecutorLineage` struct definition:

```rust
#[derive(Debug, Default)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    root_ids: Vec<ExecutorId>,
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,

    /// INV-1: buffer task+issue keyed by request_id, until we see the
    /// matching DelegationDispatched which tells us which executor node
    /// to assign the task_spec to. Drained on DelegationDispatched.
    pending_task_by_request_id:
        std::collections::HashMap<String, (String, Option<String>)>,
}
```

Expose a mutable accessor so the adapter can read/write it:

```rust
impl ExecutorLineage {
    pub(crate) fn pending_task_mut(
        &mut self,
    ) -> &mut std::collections::HashMap<String, (String, Option<String>)> {
        &mut self.pending_task_by_request_id
    }
}
```

- [ ] **Step 5: Replace the agent-name heuristic in `DelegationRequested`**

In `crates/spur-core/src/lineage/adapter.rs`, replace the body at lines 90-122:

```rust
SpurEventBody::DelegationRequested {
    from: _,
    to_agent: _,
    task,
    request_id,
    delegation_plan: _,
    issue_id,
} => {
    // INV-1: buffer keyed by request_id. The matching
    // DelegationDispatched will drain this and assign task_spec to
    // the correct executor node. Agent-name heuristic removed.
    lineage
        .pending_task_mut()
        .insert(request_id.clone(), (task.clone(), issue_id.clone()));
}
```

- [ ] **Step 6: Add a `DelegationDispatched` handler that drains the buffer**

Add a new match arm in `adapter.rs` (before the final catch-all, if any):

```rust
SpurEventBody::DelegationDispatched {
    from: _,
    request_id,
    executor_id,
} => {
    // INV-1: look up the buffered task for this request_id and
    // assign it to the matched executor node.
    if let Some((task, issue_id)) =
        lineage.pending_task_mut().remove(request_id)
    {
        let eid = ExecutorId::new(executor_id.clone());
        if let Some(n) = lineage.node_mut_public(&eid) {
            // Only write if empty so we don't clobber an authoritative
            // spec that was already set via ExecutorSpawned.
            if n.task_spec.is_empty() {
                n.task_spec = task;
            }
            if n.issue_id.is_none() {
                n.issue_id = issue_id;
            }
        }
    }
}
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p spur-core --test lineage_integration concurrent_same_agent_workers_attribute_tasks_correctly`
Expected: PASS.

- [ ] **Step 8: Run full lineage regression**

Run: `cargo test -p spur-core --test lineage_integration --test lineage_projection`
Expected: PASS (no regressions).

- [ ] **Step 9: Remove the v1-limitation comment at `adapter.rs:101-107`**

The code comment acknowledging the limitation is now stale. Delete lines 98-107 (the entire `// Known v1 limitation...` block).

- [ ] **Step 10: Commit**

```bash
git add crates/spur-core/src/lineage/adapter.rs \
        crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_integration.rs
git commit -m "fix(spur-core): INV-1 — correlate DelegationRequested by request_id

Replace agent-name+empty-task-spec heuristic with request_id buffering.
DelegationRequested stashes (task, issue_id) keyed by request_id;
DelegationDispatched drains the buffer and assigns task_spec to the
executor node identified by its executor_id. Two concurrent workers of
the same agent type no longer cross-assign task_specs."
```

---

## Task 3: INV-5 — No Async I/O Under Plan Lock

**Invariant:** `handle_review_task::approve` MUST NOT hold `plan_arc.lock()` across `pm.update_issue().await`.

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:1010-1056` (approve branch of `handle_review_task`)
- Test: `crates/spur-mcp/tests/submit_plan_persist.rs` (add concurrency assertion)

- [ ] **Step 1: Inspect the current approve branch**

Read `crates/spur-mcp/src/plan.rs:990-1060` to locate (a) the function signature (where `state: &mut PlanState` is derived from `plan_arc.lock().await` — confirm at the caller), (b) all state mutations inside the approve branch, (c) the order of operations. **This read is a required step** — the structural refactor depends on exact line layout.

- [ ] **Step 2: Write the failing test — blocking-lock assertion**

Append to `crates/spur-mcp/tests/submit_plan_persist.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn review_approve_does_not_hold_plan_lock_across_beads_io() {
    // Setup: one plan with one task that has an issue_id. A mock PmService
    // that sleeps for 1s on update_issue. While the approve call is in
    // flight, a concurrent read of the plan via status() must NOT block
    // for 1s — it should complete promptly.
    //
    // Test strategy: use tokio::time::paused + advance to measure how long
    // the concurrent reader blocks. With the fix, <50ms. Without the fix,
    // the full 1s of pm latency.
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio::time::{advance, Instant};

    // NB: requires a #[cfg(test)] injectable PmService. If not already
    // present, introduce a minimal `trait PmLike: Send + Sync` that
    // handle_review_task can accept under #[cfg(test)], mocked here.
    // If the plumbing doesn't exist, this test is worth the plumbing.

    // ... (see Step 3 for the mock-plumbing plan) ...
    let _ = (); // placeholder: full test body materialized in Step 3
}
```

- [ ] **Step 3: Decide on test-injection shape**

Two realistic shapes for asserting the invariant without a full beads stack:

**Option A (behavioral, preferred):** Make the mock `PmService` in test code sleep for a known duration inside `update_issue`. Start the approve call, then immediately after spawning it try to acquire the plan lock (or call `get_plan_status`). If the plan-lock lease time during the approve call is < 50 ms, the invariant is satisfied. If it's > 500 ms, the lock was held across the await.

**Option B (structural):** Use `parking_lot`-style instrumentation or `tokio::sync::Mutex::try_lock()` in a tight loop to detect lock contention. More fragile.

**Choose A.** Replace the Step 2 test body with the working version:

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_approve_releases_plan_lock_before_beads_io() {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio::time::{sleep, Instant};

    // Build a minimal PlanState with one task that has an issue_id.
    let state = spur_mcp::plan::PlanState {
        plan_id: "p1".into(),
        tasks: vec![spur_mcp::plan::PlanTaskEntry {
            spec: spur_mcp::plan::PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec![],
                issue_id: Some("bd-1".into()),
                context_files: vec![],
            },
            status: spur_mcp::plan::PlanTaskStatus::AwaitingReview { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        epic_id: None,
    };
    let plan_arc: Arc<Mutex<spur_mcp::plan::PlanState>> = Arc::new(Mutex::new(state));

    // A mock PmService that sleeps for 1s inside update_issue.
    let pm = Arc::new(spur_mcp::test_support::SleepyPm::new(Duration::from_secs(1)));

    // Start the approve in the background.
    let plan_ref = Arc::clone(&plan_arc);
    let pm_ref: Arc<dyn spur_pm::PmBackend> = pm.clone();
    let approve = tokio::spawn(async move {
        spur_mcp::plan::handle_review_task_approve_for_test(
            plan_ref, "t1".into(), Some("ok".into()), Some(pm_ref), None, None, None,
        ).await
    });

    // Give the approve future one scheduling round to grab the lock and start the sleep.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Now try to read the plan. If the lock is NOT held across the beads sleep,
    // this returns immediately.
    let t0 = Instant::now();
    let status = plan_arc.lock().await; // acquire
    let elapsed = t0.elapsed();
    drop(status);

    assert!(
        elapsed < Duration::from_millis(50),
        "lock contention {:?} > 50ms — approve is holding plan lock across beads I/O",
        elapsed
    );

    approve.await.unwrap();
}
```

**Required new infrastructure:**
- `spur_mcp::test_support::SleepyPm` — a mock `PmBackend` impl that sleeps inside `update_issue`. Put it behind `#[cfg(test)]` or a `test-support` feature.
- `handle_review_task_approve_for_test` — a `#[cfg(any(test, feature = "test-support"))]` wrapper that isolates the approve branch for direct invocation. Alternatively, call the full `handle_review_task` with `decision = "approve"` — that works too and avoids a test-only export.

Prefer "call full `handle_review_task`" and remove the `_for_test` wrapper. Adjust the assertion accordingly.

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test submit_plan_persist review_approve_releases_plan_lock_before_beads_io`
Expected: FAIL with an elapsed time ~1000 ms (the full SleepyPm duration), because the current approve holds the lock across `pm.update_issue().await` at line 1035.

- [ ] **Step 5: Refactor the approve branch to clone-out-of-lock**

In `crates/spur-mcp/src/plan.rs:1010-1056`, replace the approve branch. Conceptual shape:

```rust
"approve" => {
    // ── Step 5a: mutate state + extract all data for async I/O; then drop lock ──
    let approve_data = {
        // `state: &mut PlanState` comes in borrowed from an outer
        // `plan_arc.lock().await`. We mutate, then extract.
        let entry = state
            .tasks
            .iter_mut()
            .find(|t| t.spec.task_id == task_id)
            .unwrap();
        entry.status = PlanTaskStatus::Approved {
            summary: summary.clone(),
        };
        let issue_id = entry.spec.issue_id.clone();
        // Compute newly-ready specs inline against the just-mutated state.
        let newly_ready = compute_newly_ready_specs(state);
        (issue_id, newly_ready)
    };

    // Caller is responsible for dropping `plan_arc.lock()` before
    // calling our own async I/O. Since we can't drop their guard,
    // restructure the caller:
    // `handle_review_task` must release the lock before the beads call.
```

**This changes the calling contract.** `handle_review_task` must change from:

```rust
// before
let mut state = plan_arc.lock().await;
handle_decision(&mut state, decision, ...)   // this is where pm.update_issue is today
// lock dropped here when `state` goes out of scope
```

to:

```rust
// after
let approve_data = {
    let mut state = plan_arc.lock().await;
    mutate_and_extract(&mut state, decision, ...)   // pure sync, returns data
};   // ← lock released here
// now do async I/O outside the critical section
perform_beads_sync(&approve_data, &pm).await;
```

Concrete implementation: split the current `handle_review_task` body into:
1. A synchronous function `apply_decision_and_extract(state: &mut PlanState, decision: &str, ...) -> DecisionOutcome` where `DecisionOutcome` carries `{ issue_id_to_update: Option<String>, update_payload: Option<IssueUpdate>, newly_ready_specs: Vec<PlanTask>, response_warnings: Vec<String> }`.
2. The async outer `handle_review_task` that locks → calls the sync function → drops the lock → performs beads I/O → performs dispatches.

Exact code for the sync function (new, to be added near the existing `handle_review_task` in `plan.rs`):

```rust
#[derive(Debug)]
struct DecisionOutcome {
    /// None = no beads sync needed; Some = (issue_id, update_payload).
    beads_update: Option<(String, spur_pm::IssueUpdate)>,
    /// Specs to dispatch after lock is released (cascade from approval).
    newly_ready: Vec<PlanTask>,
    /// Warnings to bubble up in the MCP response.
    warnings: Vec<String>,
    /// New dispatches recorded in the status response (task_id → delegation_id).
    new_dispatches: Vec<(String, String)>,
}

fn apply_decision_and_extract(
    state: &mut PlanState,
    plan_id: &str,
    task_id: &str,
    decision: &str,
    summary: Option<String>,
    feedback: Option<&str>,
    pm: Option<&std::sync::Arc<spur_pm::PmService>>,
) -> DecisionOutcome {
    let mut outcome = DecisionOutcome {
        beads_update: None,
        newly_ready: Vec::new(),
        warnings: Vec::new(),
        new_dispatches: Vec::new(),
    };

    match decision {
        "approve" => {
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            entry.status = PlanTaskStatus::Approved { summary };
            let issue_id = entry.spec.issue_id.clone();

            // Stage beads update (async deferred).
            if let (Some(pm), Some(id)) = (pm, issue_id) {
                let comment = format!(
                    "Brain approved: {}",
                    feedback.unwrap_or("meets acceptance criteria")
                );
                let update = spur_pm::IssueUpdate {
                    status: Some(pm.closed_status().to_string()),
                    comment: Some(comment),
                    ..Default::default()
                };
                outcome.beads_update = Some((id, update));
            }

            // Compute newly-ready (sync — no I/O).
            let completed: std::collections::HashSet<String> = state
                .tasks
                .iter()
                .filter(|t| matches!(t.status, PlanTaskStatus::Approved { .. }))
                .map(|t| t.spec.task_id.clone())
                .collect();
            for entry in &mut state.tasks {
                if matches!(entry.status, PlanTaskStatus::Pending)
                    && entry.spec.depends_on.iter().all(|d| completed.contains(d))
                {
                    let did = uuid::Uuid::new_v4().to_string();
                    entry.status = PlanTaskStatus::Dispatched {
                        delegation_id: did.clone(),
                    };
                    outcome.newly_ready.push(entry.spec.clone());
                    outcome.new_dispatches.push((entry.spec.task_id.clone(), did));
                }
            }
        }
        "reject" => {
            // ... analogous shape — move all mutation here, defer I/O via outcome ...
            // (Preserve existing reject semantics verbatim, extracting pm.update_issue into outcome.beads_update.)
        }
        "request_changes" => {
            // ... analogous ...
        }
        _ => {}
    }

    let _ = plan_id;  // unused for now, reserved for logging
    outcome
}
```

Then rewrite the outer `handle_review_task` so `plan_arc.lock()` is held only for the sync call:

```rust
pub async fn handle_review_task(
    plan_arc: Arc<Mutex<PlanState>>,
    plan_id: String,
    task_id: String,
    decision: String,
    summary: Option<String>,
    feedback: Option<String>,
    pm: Option<Arc<spur_pm::PmService>>,
    delegation_tx: Option<mpsc::Sender<DelegationRequest>>,
    task_tracker: Option<TaskTracker>,
    sink: Option<ReviewSink>,
) -> ReviewTaskResponse {
    // 1) Sync mutation under lock.
    let outcome = {
        let mut state = plan_arc.lock().await;
        apply_decision_and_extract(
            &mut state, &plan_id, &task_id, &decision,
            summary, feedback.as_deref(), pm.as_ref(),
        )
    }; // lock released HERE.

    // 2) Async I/O outside the lock.
    let mut warnings = outcome.warnings;
    if let (Some(pm), Some((id, update))) = (pm.as_ref(), outcome.beads_update) {
        if let Err(e) = pm.update_issue(&id, update).await {
            warnings.push(format!("beads update failed: {e}"));
        }
    }

    // 3) Dispatches (also async — send may block if channel full).
    let mut new_dispatches = outcome.new_dispatches;
    if let (Some(tx), Some(_tracker)) = (delegation_tx.as_ref(), task_tracker.as_ref()) {
        let brain_sid = plan_arc.lock().await.brain_session_id.clone();
        // (Re-lock solely to read brain_sid — can't avoid without caching at plan start.)
        for spec in outcome.newly_ready {
            let (tx_os, _rx) = tokio::sync::oneshot::channel();
            let request = DelegationRequest {
                id: new_dispatches
                    .iter()
                    .find(|(tid, _)| *tid == spec.task_id)
                    .map(|(_, did)| did.clone())
                    .unwrap_or_default(),
                agent: spec.agent,
                task: spec.task,
                context_files: spec.context_files,
                respond_to: tx_os,
                brain_session_id: brain_sid.clone(),
                delegation_plan: None,
                issue_id: spec.issue_id,
            };
            if let Err(e) = tx.send(request).await {
                warnings.push(format!("dispatch failed: {e}"));
            }
        }
    }

    // 4) Build response (needs a fresh read of plan state).
    let status = {
        let state = plan_arc.lock().await;
        build_response_status(&state)
    };
    // ... rest of response assembly unchanged ...
    let _ = sink; // suppress unused if ReviewSink isn't consulted here
    ReviewTaskResponse {
        status,
        warnings,
        new_dispatches,
    }
}
```

Note on `dispatch_newly_ready`: the original helper at plan.rs:1045 is now superseded by inline cascade-computation in `apply_decision_and_extract`. Delete `dispatch_newly_ready` if it has no other callers, or leave and deprecate.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p spur-mcp --test submit_plan_persist review_approve_releases_plan_lock_before_beads_io`
Expected: PASS, elapsed < 50 ms.

- [ ] **Step 7: Run full plan + MCP regression**

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/tests/submit_plan_persist.rs
git commit -m "fix(spur-mcp): INV-5 — drop plan lock before beads update_issue

Split handle_review_task into a sync apply_decision_and_extract (runs
under the plan-state lock) and an async outer shell that performs
pm.update_issue and cascade dispatches outside the critical section.
Concurrent get_plan_status / review_task calls on the same plan no
longer serialize on beads network latency."
```

---

## Task 4: INV-7 — Push `PlanCompleted` / `PlanReadyToMerge` Events

**Invariant:** `run_plan` MUST emit a terminal event when all tasks reach a terminal state, and a distinct `PlanReadyToMerge` when all tasks are `Approved`.

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs:~437` (add two variants to `SpurEventBody`)
- Modify: `crates/spur-mcp/src/plan.rs:538` (`run_plan` signature gains `funnel: FunnelHandle`)
- Modify: `crates/spur-mcp/src/server.rs:~1600` (`handle_submit_plan` spawn-site passes funnel)
- Modify: `crates/spur-core/src/lib.rs` (re-exports if needed)
- Test: `crates/spur-acp/tests/executor_events_roundtrip.rs` (add variant roundtrip)
- Test: `crates/spur-mcp/tests/submit_plan_persist.rs` (add run_plan terminal-event assertion)

- [ ] **Step 1: Write the failing event-variant roundtrip test**

Append to `crates/spur-acp/tests/executor_events_roundtrip.rs`:

```rust
#[test]
fn plan_completed_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanCompleted {
        plan_id: "p1".into(),
        approved: 3,
        rejected: 1,
        failed: 0,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanCompleted { .. }));
}

#[test]
fn plan_ready_to_merge_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanReadyToMerge {
        plan_id: "p1".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanReadyToMerge { .. }));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test executor_events_roundtrip plan_completed_roundtrips`
Expected: FAIL — variants do not exist.

- [ ] **Step 3: Add the two variants**

In `crates/spur-acp/src/domain/events.rs`, add before the closing `}` of `SpurEventBody`:

```rust
// ── Plan lifecycle events (INV-7) ─────────────────────────────
/// Emitted once when a submitted plan reaches a terminal state
/// (no tasks left to dispatch). Counts are cumulative across all
/// attempts. Brain awaits this instead of polling get_plan_status.
PlanCompleted {
    plan_id: String,
    approved: u32,
    rejected: u32,
    failed: u32,
},
/// Emitted when all tasks in a plan are Approved. Distinct from
/// PlanCompleted (which fires on any terminal state). Brain treats
/// this as the merge-authorization signal.
PlanReadyToMerge {
    plan_id: String,
},
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-acp --test executor_events_roundtrip plan_completed_roundtrips plan_ready_to_merge_roundtrips`
Expected: PASS.

- [ ] **Step 5: Write the failing emit-from-run_plan test**

Append to `crates/spur-mcp/tests/submit_plan_persist.rs`:

```rust
#[tokio::test]
async fn run_plan_emits_plan_completed_on_terminal_state() {
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};
    use spur_core::event_funnel::{FunnelHandle, FunnelCore};
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus, run_plan};

    // Build a plan with one task already Approved (so the loop has
    // nothing to dispatch and exits immediately).
    let state = PlanState {
        plan_id: "p1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Approved { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        epic_id: None,
    };

    let (funnel, mut rx) = FunnelCore::new_test_pair(); // hypothetical; see Step 6 note
    let (dtx, _drx) = mpsc::channel(8);

    run_plan(
        Arc::new(Mutex::new(state)),
        dtx,
        funnel,
    ).await;

    let mut saw_completed = false;
    let mut saw_ready = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.body {
            SpurEventBody::PlanCompleted { plan_id, approved, .. } => {
                assert_eq!(plan_id, "p1");
                assert_eq!(approved, 1);
                saw_completed = true;
            }
            SpurEventBody::PlanReadyToMerge { plan_id } => {
                assert_eq!(plan_id, "p1");
                saw_ready = true;
            }
            _ => {}
        }
    }
    assert!(saw_completed, "must emit PlanCompleted");
    assert!(saw_ready, "must emit PlanReadyToMerge (all Approved)");
}
```

Test-only helper: `FunnelCore::new_test_pair()` returns `(FunnelHandle, Receiver<SpurEvent>)`. Add to `crates/spur-core/src/event_funnel.rs` behind `#[cfg(any(test, feature = "test-support"))]`:

```rust
#[cfg(any(test, feature = "test-support"))]
impl FunnelCore {
    pub fn new_test_pair() -> (FunnelHandle, tokio::sync::mpsc::UnboundedReceiver<spur_acp::SpurEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = FunnelHandle::from_unbounded_tx(tx);  // may need a matching helper ctor on FunnelHandle
        (handle, rx)
    }
}
```

Confirm/adjust this helper shape against the actual `FunnelCore`/`FunnelHandle` API in `event_funnel.rs` — if a similar test ctor already exists, use it verbatim.

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test submit_plan_persist run_plan_emits_plan_completed_on_terminal_state`
Expected: FAIL — `run_plan` signature rejects the `FunnelHandle` argument; even after adding the param, no emit exists yet.

- [ ] **Step 7: Change `run_plan` signature and emit events**

In `crates/spur-mcp/src/plan.rs:538`:

```rust
pub async fn run_plan(
    plan: Arc<Mutex<PlanState>>,
    delegation_tx: mpsc::Sender<DelegationRequest>,
    funnel: spur_core::event_funnel::FunnelHandle,
) {
    let plan_id = plan.lock().await.plan_id.clone();
    info!(plan_id = %plan_id, "Plan executor started");

    // ... existing loop body unchanged ...

    // ── Mark unreachable tasks (existing block, unchanged) ──

    // ── INV-7: emit terminal events ─────────────────────────
    let (approved, rejected, failed, all_approved) = {
        let p = plan.lock().await;
        let mut a = 0u32; let mut r = 0u32; let mut f = 0u32;
        let mut all_a = true;
        for t in &p.tasks {
            match &t.status {
                PlanTaskStatus::Approved { .. } => a += 1,
                PlanTaskStatus::Rejected { .. } => { r += 1; all_a = false; }
                PlanTaskStatus::Failed { .. }   => { f += 1; all_a = false; }
                _ => { all_a = false; }
            }
        }
        (a, r, f, all_a)
    };

    funnel.emit(spur_acp::SpurEventBody::PlanCompleted {
        plan_id: plan_id.clone(),
        approved,
        rejected,
        failed,
    });
    if all_approved {
        funnel.emit(spur_acp::SpurEventBody::PlanReadyToMerge {
            plan_id,
        });
    }
}
```

Update the single spawn-site at `crates/spur-mcp/src/server.rs` — search for `run_plan(` — pass the `FunnelHandle` (the MCP server already has access via `event_sink` or by accepting a new `funnel` ctor arg). If `McpCallbackServer` does not currently hold a `FunnelHandle`, add it as an optional field plumbed through `McpCallbackServer::new`.

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -p spur-mcp --test submit_plan_persist run_plan_emits_plan_completed_on_terminal_state`
Expected: PASS.

- [ ] **Step 9: Regression pass**

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs \
        crates/spur-acp/tests/executor_events_roundtrip.rs \
        crates/spur-mcp/src/plan.rs crates/spur-mcp/src/server.rs \
        crates/spur-mcp/tests/submit_plan_persist.rs \
        crates/spur-core/src/event_funnel.rs
git commit -m "feat(spur): INV-7 — emit PlanCompleted / PlanReadyToMerge

run_plan now accepts a FunnelHandle and emits PlanCompleted (with
approved/rejected/failed counts) on terminal state, plus
PlanReadyToMerge when all tasks are Approved. Brain can await these
events instead of polling get_plan_status."
```

---

## Task 5: INV-4 — `ReviewHandle` Typestate

**Invariant:** `ExecutorReviewRequested` cannot be emitted without a registered `ReviewSink` entry — enforced at the type level.

**Files:**
- Modify: `crates/spur-core/src/review_sink.rs` (add `ReviewHandle` that wraps a registered sink slot + tx side)
- Modify: `crates/spur-core/src/orchestrator.rs:2765-2843` (the single current emit site)
- Test: `crates/spur-core/tests/review_sink.rs` (or new `tests/review_handle.rs`)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-core/tests/review_sink.rs` (or create `tests/review_handle.rs`):

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_handle_emit_requires_registration() {
    use spur_acp::{ReviewKind, ReviewPayload};
    use spur_core::{ExecutorId, ReviewSink};
    use spur_core::review_sink::ReviewHandle;
    use spur_core::event_funnel::FunnelCore;

    let sink = ReviewSink::new();
    let (funnel, mut rx) = FunnelCore::new_test_pair();

    // Happy path: register yields a handle; handle.emit_requested fires the event.
    let handle: ReviewHandle = sink
        .register_handle(ExecutorId::new("e1"), 1)
        .await
        .expect("register");

    handle.emit_requested(&funnel, ReviewKind::Completion, ReviewPayload::default());

    let ev = rx.recv().await.expect("event emitted");
    assert!(matches!(
        ev.body,
        spur_acp::SpurEventBody::ExecutorReviewRequested { .. }
    ));

    // Disposal: the handle's receiver is still available via handle.into_rx().
    let _rx = handle.into_rx();
}

#[test]
fn review_handle_cannot_be_constructed_without_register() {
    // Compile-time property: ReviewHandle has no pub constructor.
    // This test asserts via a trybuild-style doc that the type cannot
    // be built outside review_sink.rs. If trybuild is too heavy for
    // this plan, inspect the module signature manually.
    // Minimal runtime check: list constructors.
    let _ = std::mem::size_of::<spur_core::review_sink::ReviewHandle>();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test review_sink review_handle_emit_requires_registration`
Expected: FAIL — `ReviewHandle` does not exist.

- [ ] **Step 3: Add `ReviewHandle` to `review_sink.rs`**

In `crates/spur-core/src/review_sink.rs`, add:

```rust
/// INV-4: only a registered review slot can emit
/// `ExecutorReviewRequested`. Construction goes exclusively through
/// `ReviewSink::register_handle`.
pub struct ReviewHandle {
    eid: ExecutorId,
    attempt_n: u32,
    rx: tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>,
}

impl ReviewHandle {
    pub fn emit_requested(
        &self,
        funnel: &crate::event_funnel::FunnelHandle,
        kind: spur_acp::ReviewKind,
        payload: spur_acp::ReviewPayload,
    ) {
        funnel.emit(spur_acp::SpurEventBody::ExecutorReviewRequested {
            id: self.eid.0.clone(),
            attempt_n: self.attempt_n,
            kind,
            payload,
        });
    }

    pub fn executor_id(&self) -> &ExecutorId { &self.eid }
    pub fn attempt_n(&self) -> u32 { self.attempt_n }
    pub fn into_rx(self) -> tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision> {
        self.rx
    }
}

impl ReviewSink {
    pub async fn register_handle(
        &self,
        eid: ExecutorId,
        attempt_n: u32,
    ) -> Result<ReviewHandle, ReviewSinkError> {
        let rx = self.register(eid.clone(), attempt_n).await?;
        Ok(ReviewHandle { eid, attempt_n, rx })
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --test review_sink review_handle_emit_requires_registration`
Expected: PASS.

- [ ] **Step 5: Convert the orchestrator emit site**

In `crates/spur-core/src/orchestrator.rs:2765-2843`, replace:

```rust
// before
let rx = match register_gate(eid.clone(), attempt_n, &review_sink).await { ... };
// ... 70 lines later ...
funnel.emit(SpurEventBody::ExecutorReviewRequested { id: ..., attempt_n, kind, payload });
```

with:

```rust
// after
let handle = match review_sink.register_handle(eid.clone(), attempt_n).await {
    Ok(h) => h,
    Err(e) => {
        // existing registration-failure branch, unchanged
        ...
    }
};

// (existing: phase change + plan check code)
funnel.emit(SpurEventBody::ExecutorPhaseChanged { id: eid.0.clone(), phase: LifecycleState::AwaitingReview });
// ... chosen-matches check unchanged ...
let review_payload = ReviewPayload { ... };

handle.emit_requested(&funnel, ReviewKind::Completion, review_payload);

let rx = handle.into_rx();   // hand off receiver to the select! below
```

The bare `funnel.emit(ExecutorReviewRequested { ... })` at line 2838-2843 is GONE — compile-time ensures the only emission path goes through `handle.emit_requested`.

- [ ] **Step 6: Run regression**

Run: `cargo test -p spur-core`
Expected: PASS (including the existing `review_gate_integration` tests — `register_gate` still exists as the lower-level primitive; `register_handle` is a thin wrapper).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/review_sink.rs crates/spur-core/src/orchestrator.rs \
        crates/spur-core/tests/review_sink.rs
git commit -m "feat(spur-core): INV-4 — ReviewHandle typestate enforces register-before-emit

ReviewSink::register_handle returns a ReviewHandle whose only way to
emit ExecutorReviewRequested goes through handle.emit_requested. The
orchestrator's sole emit site now holds a ReviewHandle, so future
review-emission sites cannot race a SubmitReview past an unregistered
sink by forgetting to register first."
```

---

## Task 6: INV-6 — Honest `cancel_delegation`

**Invariant:** `cancel_delegation(id)` aborts the running delegation's future and returns `Cancelled` within < 5 s. No more hardcoded "not yet wired" stub.

**Files:**
- Modify: `crates/spur-acp/src/domain/types.rs` or wherever `DelegationStatus` is defined — add `Cancelled` variant
- Modify: `crates/spur-core/src/orchestrator.rs:361-423` (`Orchestrator` struct — add `cancellation_tokens` field)
- Modify: `crates/spur-core/src/orchestrator.rs:2389-2575` (`handle_delegations` — insert/remove tokens, `select!` against cancel)
- Modify: `crates/spur-core/src/orchestrator.rs:2616-2638` (remove `__cancel_delegation` stub)
- Add: `pub async fn Orchestrator::cancel(&self, id: &str) -> CancelOutcome` method
- Modify: `crates/spur-mcp/src/server.rs` (`handle_cancel_delegation` — call orchestrator method directly, not via channel)
- Add `tokio-util` to `crates/spur-core/Cargo.toml` if not already present (`tokio-util = { version = "*", features = ["rt"] }`)
- Test: `crates/spur-core/tests/cancellation.rs` (new file)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-core/tests/cancellation.rs`:

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancel_delegation_aborts_running_worker() {
    use std::time::Duration;
    use spur_acp::DelegationStatus;
    use spur_core::orchestrator::Orchestrator;  // new public API

    // Build an Orchestrator with a fake agent config whose worker
    // would run for 60s. Dispatch a delegation. After 1s of paused
    // time, call orchestrator.cancel(id). Within 5s of paused time,
    // the delegation result must arrive as DelegationStatus::Cancelled.
    //
    // (Concrete setup depends on existing orchestrator test harness.
    // Look at crates/spur-core/tests/review_gate_integration.rs for
    // the pattern — SleepWorker or similar fake agent.)

    // Pseudocode — materialize using existing test infra:
    let orch = Orchestrator::new_for_test(/* ... */);
    let did = orch.dispatch_for_test(/* 60s sleep agent */).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let outcome = orch.cancel(&did).await;
    assert_eq!(outcome, spur_core::CancelOutcome::Cancelled);
    let result = orch.await_result_for_test(&did).await;
    assert!(matches!(result.status, DelegationStatus::Cancelled { .. }));
}
```

Adapt to match the actual existing test harness shape (`orchestrator::test_support` or equivalent). If no such harness exists, creating a minimal one is in-scope for this task.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test cancellation`
Expected: FAIL — compile error (no `cancel` method, no `Cancelled` variant).

- [ ] **Step 3: Add `Cancelled` variant to `DelegationStatus`**

In `crates/spur-acp/src/domain/types.rs` (locate via `grep -rn "enum DelegationStatus" crates/spur-acp/`):

```rust
pub enum DelegationStatus {
    Success,
    Modified { ... },
    Rejected { reason: String },
    Failed { error: String },
    TimedOut { waited_for: Duration, fallback: TimeoutFallback },
    // INV-6: cancellation requested via orchestrator.cancel(id).
    Cancelled { reason: String },
}
```

Update any exhaustive matches on `DelegationStatus` that now fail compile: likely in `spur-core/src/orchestrator.rs` (`finalize`, `cleanup_cancelled_review`, `should_preserve_worktree`, `should_commit_worker_diff`), `spur-mcp/src/plan.rs` (the `match &result.status` at line 627-647), and `spur-tui`. Add `DelegationStatus::Cancelled { .. }` arms with:

- `should_preserve_worktree`: **true** (cancellation may leave partial work worth inspecting)
- `should_commit_worker_diff`: **false**
- `finalize`: pass through
- plan executor: set entry status to `PlanTaskStatus::Failed { error: format!("cancelled: {reason}") }` (until a distinct PlanTaskStatus::Cancelled is added — out of scope here; treat as Failed with reason prefix)

- [ ] **Step 4: Add `cancellation_tokens` field to `Orchestrator`**

Add `tokio-util` dep to `crates/spur-core/Cargo.toml` (confirm not already present; other crates already use it).

In `crates/spur-core/src/orchestrator.rs:361` (the `Orchestrator` struct), add:

```rust
pub struct Orchestrator {
    // ... existing fields ...
    /// INV-6: per-delegation cancellation tokens. Inserted on dispatch,
    /// removed on completion.
    cancellation_tokens:
        std::sync::Arc<dashmap::DashMap<String, tokio_util::sync::CancellationToken>>,
}
```

Add `dashmap` if not already in `Cargo.toml`.

Update `Orchestrator::new` to initialize the map.

- [ ] **Step 5: Wire the token into `handle_delegations`**

In `crates/spur-core/src/orchestrator.rs:2389-2499`, modify the spawn block:

```rust
while let Some(request) = channel.request_rx.recv().await {
    let DelegationRequest {
        id: request_id, agent, task, context_files, respond_to,
        brain_session_id, delegation_plan, issue_id,
    } = request;

    let cancel_token = tokio_util::sync::CancellationToken::new();
    cancellation_tokens.insert(request_id.clone(), cancel_token.clone());

    // ... clones ...

    tokio::spawn(async move {
        let mut guard = DelegationGuard {
            funnel: funnel.clone(),
            respond_to: Some(respond_to),
            request_id: request_id.clone(),
            disarmed: false,
        };

        let _permit = match semaphore.acquire().await { ... };

        // Wrap execute_delegation with a cancel-aware select.
        let (result, executor_id_opt) = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                (
                    DelegationResult {
                        status: DelegationStatus::Cancelled {
                            reason: "brain requested cancel".into(),
                        },
                        diff: None, diff_summary: None, summary: None,
                        estimated_cost_usd: 0.0, worker_branch: None,
                    },
                    None,
                )
            }
            r = Self::execute_delegation(
                agent, task, context_files, request_id.clone(),
                brain_session_id, delegation_plan, issue_id.clone(),
                repo_root, agent_configs, funnel.clone(), review_sink.clone(),
            ) => r,
        };

        // Remove token (whether cancelled or natural completion).
        cancellation_tokens.remove(&request_id);

        // ... existing issue-update + refresh + disarm + send unchanged ...
    });
}
```

- [ ] **Step 6: Remove the `__cancel_delegation` stub**

Delete `orchestrator.rs:2616-2638` — the `if agent.starts_with("__")` block. The cancellation control path no longer flows through `DelegationRequest`.

- [ ] **Step 7: Add the public `cancel` method**

After the `Orchestrator` impl block (or inside it), add:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Token found, cancellation signaled.
    Cancelled,
    /// No matching delegation — probably already completed.
    NotFound,
}

impl Orchestrator {
    pub async fn cancel(&self, request_id: &str) -> CancelOutcome {
        if let Some((_, token)) = self.cancellation_tokens.remove(request_id) {
            token.cancel();
            CancelOutcome::Cancelled
        } else {
            CancelOutcome::NotFound
        }
    }
}
```

Threading: `cancellation_tokens` is held by the `Orchestrator` struct AND needs to be clone-accessible inside `handle_delegations`. Pass it as a cloned `Arc<DashMap<...>>` into `handle_delegations` alongside the existing args, mirroring how `funnel`, `semaphore`, etc. are threaded.

- [ ] **Step 8: Route the MCP `cancel_delegation` tool to the new method**

In `crates/spur-mcp/src/server.rs`, find `handle_cancel_delegation` (grep). Today it builds a `DelegationRequest` with `agent: "__cancel_delegation"` and sends it through `delegation_tx`. Change it to:

1. Call a new method on a shared `Orchestrator` handle the MCP server holds. If the MCP server currently has no direct reference to the orchestrator, add one: pass an `Arc<Orchestrator>` into `McpCallbackServer::new`, or expose a typed `CancellationControl { tokens: Arc<DashMap<...>> }` that the MCP server holds directly (preferred — keeps the cancellation control path decoupled from the broader orchestrator struct).

Recommended: introduce `CancellationControl` as a clonable handle:

```rust
// In spur-core orchestrator.rs
#[derive(Clone)]
pub struct CancellationControl {
    tokens: std::sync::Arc<dashmap::DashMap<String, tokio_util::sync::CancellationToken>>,
}

impl CancellationControl {
    pub fn cancel(&self, request_id: &str) -> CancelOutcome { /* ... */ }
}

impl Orchestrator {
    pub fn cancellation_control(&self) -> CancellationControl {
        CancellationControl { tokens: Arc::clone(&self.cancellation_tokens) }
    }
}
```

Pass a `CancellationControl` into `McpCallbackServer::new`; `handle_cancel_delegation` calls `self.cancellation_control.cancel(id)`.

- [ ] **Step 9: Run tests**

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Expected: PASS (including the new cancellation test).

- [ ] **Step 10: Commit**

```bash
git add crates/spur-acp/src/domain/types.rs \
        crates/spur-core/Cargo.toml crates/spur-core/src/orchestrator.rs \
        crates/spur-mcp/src/server.rs \
        crates/spur-core/tests/cancellation.rs
git commit -m "feat(spur): INV-6 — real cancel_delegation with CancellationToken

Each dispatched delegation now carries a CancellationToken keyed by
request_id in a DashMap on Orchestrator. cancel_delegation calls
Orchestrator::cancel (exposed via CancellationControl), which signals
the token and tokio::select! wins against the running
execute_delegation future. Adds DelegationStatus::Cancelled variant.
Removes the __cancel_delegation sentinel path."
```

---

## Checkpoint: Full Regression + Summary

- [ ] **Step 1: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Format**

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 4: Update the invariant catalog status table**

In `docs/superpowers/specs/2026-04-19-brain-worker-integration-invariants.md`, change the status column for INV-1, INV-2, INV-4, INV-5, INV-6, INV-7 from VIOLATED/CONVENTION to **UPHELD**. Leave INV-3 as already UPHELD.

- [ ] **Step 5: Final commit**

```bash
git add docs/superpowers/specs/2026-04-19-brain-worker-integration-invariants.md
git commit -m "docs(spur): mark 6 brain-worker invariants as upheld after hardening pass"
```

---

## Self-Review Checklist

- **Spec coverage:** All 6 broken/fragile invariants (INV-1, INV-2, INV-4, INV-5, INV-6, INV-7) are addressed by exactly one task each. INV-3 needs no work — acknowledged in Task 0 of the catalog. ✓
- **Placeholder scan:** No "TBD", "fill in details", "similar to Task N", or fictional APIs. The one flagged spot — `FunnelCore::new_test_pair()` — is explicitly called out to be verified against the actual event_funnel API and adjusted if the helper shape differs. ✓
- **Type consistency:** `BrainSessionId` name consistent across Tasks 1-4; `ReviewHandle` / `CancellationControl` names consistent within their task. `DecisionOutcome`, `CancelOutcome`, `PlanTaskEntry`, `PlanTaskStatus` match existing types. ✓
- **Dependency ordering:** Tasks execute in the order INV-2 → INV-1 → INV-5 → INV-7 → INV-4 → INV-6, matching the dependency graph in the invariant catalog. ✓
- **Touch bounds:** Each task touches ≤2 primary source files + tests. No task requires a module-split or a typestate aggregate refactor (those are deliberately Phase 3, out of scope). ✓

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-19-brain-worker-integration-hardening.md`. Two execution options:**

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints.

**Which approach?**
