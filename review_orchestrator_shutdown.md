# MCTS + First-Principles Review: Worker Delegation & Shutdown Reliability

## Review Target
`crates/spur-core/src/orchestrator.rs` — delegation task dispatch, worker lifecycle, and brain-worker shutdown coupling.

## Methodology
- **Round 1 (Selection/Exploration)**: Breadth-first traversal of all shutdown paths to identify anomaly surfaces.
- **Round 2 (Expansion/Exploitation)**: Deep-dive into high-impact nodes using first-principles decomposition (causality, resource ownership, event propagation guarantees).
- **Round 3 (Simulation/Backprop)**: Cross-verify findings against invariants documented in code comments and AGENTS.md signal conventions.

---

# ROUND 1: Exploration — Shutdown Path Topology

## 1.1 Brain Session Lifecycle (run_interactive)
```
BrainSession {
    connection: Box<dyn AgentConnection>,           // brain subprocess
    delegation_handle: JoinHandle<()>,               // handle_delegations loop
    mcp_handle: AbortOnDropHandle<()>,               // HTTP server task
    notification_pump_handle: Option<JoinHandle<()>>, // broadcast→event bus pump
}
```

**Retirement path (`retire_active_brain`):**
1. Emit `BrainRetired`
2. Close cost ledger
3. Drain notification pump (100ms grace) → abort
4. `delegation_handle.abort()`
5. `abort_mcp_handle(mcp_handle)`
6. Stash `connection` for reuse

**End-of-session cleanup (`run_interactive` tail):**
1. `delegation_handle.abort()`
2. Abort notification pump
3. `abort_mcp_handle(mcp_handle)`
4. `connection.shutdown().await`

## 1.2 Ad-hoc Path (run_adhoc)
1. `connection.shutdown().await`
2. `delegation_handle.abort()`
3. `abort_mcp_handle(mcp_handle)`

## 1.3 Worker Task Lifecycle (inside handle_delegations)
```rust
while let Some(request) = channel.request_rx.recv().await {
    tokio::spawn(async move {
        let mut guard = DelegationGuard { ... };
        // ... semaphore, worktree, agent connection, prompt, diff ...
        guard.disarmed = true;
        respond_to.send(result)
    });
}
```

## 1.4 MCP Server Lifecycle
```rust
McpCallbackServer {
    delegation_tx: mpsc::Sender<DelegationRequest>,  // → handle_delegations
    task_tracker: TaskTracker,                        // result collectors
}
```

`McpCallbackServer::start()` spawns HTTP server task (wrapped in `AbortOnDropHandle`).
`McpCallbackServer::shutdown()` calls `task_tracker.close()` + `wait()` — **never invoked by orchestrator**.

---

# ROUND 2: Exploitation — First-Principles Deep Dive

## FINDING 1: Worker Orphaning — Child Tasks Survive Parent Abort (CRITICAL)

### Principle: Resource Ownership Must Be Transitive
If task A owns task B, aborting A must deterministically terminate B or provide an explicit detachment contract.

### Evidence
```rust
// orchestrator.rs:2976
while let Some(request) = channel.request_rx.recv().await {
    // ...
    tokio::spawn(async move {  // ← child task: worker execution
        // run_one_worker_attempt(...)
        // connection.shutdown().await;  // ← only reached on normal completion
    });
}
```

When `delegation_handle.abort()` fires (retire_active_brain, run_adhoc cleanup, error paths):
- The **parent** `handle_delegations` task is cancelled.
- The **children** (`tokio::spawn` worker tasks) are **NOT** automatically cancelled by tokio.
- Each child continues executing `run_one_worker_attempt`.
- The worker subprocess (`AgentConnection`) is only shut down at line 4391:
  ```rust
  let _ = connection.shutdown().await;
  ```
  This line is only reached if the worker completes its prompt turn **normally**.

### Consequence
**If the brain retires or the orchestrator shuts down while workers are in-flight, the worker processes become orphaned.** They continue consuming:
- CPU/memory (running LLM inference)
- Disk (worktree holds checked-out files)
- License quota (cost tracker is per-brain-session; worker costs leak)

The `CancellationControl` registry (INV-6) only handles **explicit** `cancel_delegation` tool calls. It does **not** react to implicit brain shutdown.

### Specific Trigger Scenarios
1. **User switches sessions** (`NewSessionWithMessage` → `retire_active_brain`)
2. **User resumes a different session** (`ResumeSession` → `retire_active_brain`)
3. **Ad-hoc run completes** (`run_adhoc` tail cleanup)
4. **Brain prompt dies** (`is_connection_death` → reconnect path)
5. **Auth error** → brain killed, delegation_handle aborted

In ALL these paths, in-flight workers are abandoned.

---

## FINDING 2: MCP Server Task Tracker Never Closed (CRITICAL)

### Principle: Every Spawned Task Must Have a Deterministic Join Point
A task tracker that is never closed creates a reference-count leak for all tasks spawned into it.

### Evidence
```rust
// orchestrator.rs:159-162
async fn abort_mcp_handle(handle: AbortOnDropHandle<()>) {
    handle.abort();
    let _ = handle.await;
}
```

```rust
// spur-mcp/src/server.rs:1574-1577
pub async fn shutdown(&self) {
    self.task_tracker.close();
    self.task_tracker.wait().await;
}
```

**`McpCallbackServer::shutdown()` is never called.** The orchestrator only aborts the HTTP listener task.

The HTTP server task (lines 1904-1923) contains cleanup for reconciler and signal watcher, but NOT for the `task_tracker`. The `task_tracker` lives inside the `Arc<McpCallbackServer>`, which may be held by:
- In-flight HTTP request handlers
- Result collector tasks spawned via `spawn_result_collector`
- Plan runners spawned via `spawn_ephemeral_plan_runner`

When the HTTP task is aborted:
- In-flight HTTP handlers are dropped.
- But **result collectors** already handed off to `task_tracker` keep running.
- Since `task_tracker.close()` is never called, `task_tracker.wait()` would block forever.
- Collectors hold `Arc<McpCallbackServer>`, preventing drop.
- `Arc<McpCallbackServer>` holds `delegation_tx`, keeping the mpsc channel technically open (though no one sends).

### Consequence
1. **Memory leak**: `active_delegations`, `completed_delegations`, and `active_plans` HashMaps are never dropped.
2. **Result collectors hang**: A detached collector awaits `rx` (oneshot from orchestrator). If the orchestrator aborted the delegation before the worker finished, the collector waits forever.
3. **Continuation callbacks may never fire**: The collector is responsible for calling `report_detached_completion`. If it leaks, brain continuations stall.

---

## FINDING 3: Missing Shutdown Cascade — Brain Death Does Not Kill Workers (HIGH)

### Principle: Causality Must Propagate Through Defined Channels
If the brain session ends, all work scoped to that session must receive the termination signal.

### Evidence
The `CancellationControl` mechanism:
```rust
// orchestrator.rs:3012-3021
let cancel_token = {
    let cc = cancellation_control.clone();
    cc.register(request_id.clone()).await
};
// ...
tokio::select! {
    biased;
    _ = cancel_token.cancelled() => { /* return Cancelled */ }
    r = Self::execute_delegation(...) => r,
}
```

This is ONLY triggered by `handle_cancel_delegation` (explicit tool call). There is **no** code path that calls `cancellation_control.cancel_all()` or `cancellation_control.cancel(&request_id)` during brain shutdown.

The `BrainSession` struct does not hold a reference to the `CancellationControl` registry, so `retire_active_brain` cannot iterate active delegations and cancel them.

### Consequence
Workers are session-scoped resources but are not bound to session lifetime. This violates the expected invariant that retiring a brain session terminates all subsidiary work.

---

## FINDING 4: Worker Shutdown May Hang Indefinitely (MEDIUM)

### Principle: Graceful Operations Must Have a Coercion Deadline
Any `await` on external process lifecycle must be bounded.

### Evidence
```rust
// orchestrator.rs:4391
let _ = connection.shutdown().await;
```

This is inside `run_one_worker_attempt`, which is inside the `tokio::spawn` child task. No timeout wraps this call.

Transport-specific shutdown behaviors:
- **NativeAcpConnection**: Sends `Shutdown` command to ACP thread, waits for `reply_rx.await`, then `handle.join()` (blocking thread join!).
- **StdioAdapter**: Drops stdin, waits `tokio::time::timeout(3s, child.wait())` — **has timeout**.
- **CliWrapAdapter**: `child.kill().await` — immediate.
- **StreamJsonAdapter**: `child.kill().await` — immediate.

**NativeAcpConnection shutdown can hang** if the ACP thread is deadlocked or unresponsive. The `handle.join()` is a **blocking** call inside an async context. While it's inside a `tokio::spawn` (so it blocks a worker thread, not the main thread), it prevents the worker task from completing, which prevents the `DelegationGuard` from firing and prevents worktree cleanup.

### Consequence
A hung worker process can indefinitely block:
- Worktree removal (disk leak)
- `DelegationCompleted` emission (if shutdown hangs before `finalize`)
- The child task itself (tying up a tokio worker thread)

---

## FINDING 5: Result Delivery Failure on Brain Gone (MEDIUM)

### Principle: Every Terminal State Must Emit Exactly One Terminal Event
When a worker completes but the brain has already disconnected, the result must still be accounted for.

### Evidence
```rust
// orchestrator.rs:3173-3193
guard.disarmed = true;
let respond_to = guard.respond_to.take().unwrap();

if let Err(_returned_result) = respond_to.send(result) {
    if let Some(ref eid) = executor_id_opt {
        cleanup_cancelled_review(eid, "brain call cancelled", &funnel, &review_sink).await;
    }
}
```

If `respond_to.send()` fails because the MCP server dropped the oneshot receiver:
- `DelegationCompleted` was already emitted (good).
- But the `DelegationResult` is **dropped on the floor**.
- Only `ExecutorReviewCancelled` is emitted (if a review was pending).
- If no review was pending, **nothing** happens. The brain, if it reconnects, will never see the result.

For detached completions (inline_wait timeout path), the result collector holds the `rx`. If the MCP server is aborted, the collector task leaks (Finding 2). The result is never delivered to the continuation bridge.

### Consequence
Brain continuations stall. The TUI may show a worker as "running" indefinitely if `DelegationCompleted` was missed by a lagged subscriber, and the detached completion never fires.

---

## FINDING 6: Notification Pump Race in retire_active_brain (LOW-MEDIUM)

### Principle: Grace Windows Must Be Sized to the Worst-Case Latency, Not the Happy Path
A 100ms drain window assumes the pump task is schedulable and the broadcast channel is empty.

### Evidence
```rust
// orchestrator.rs:1969-1977
if let Some(h) = b.notification_pump_handle {
    let abort = h.abort_handle();
    if tokio::time::timeout(std::time::Duration::from_millis(100), h)
        .await
        .is_err()
    {
        abort.abort();
    }
}
```

Under load, the pump task may be contending for tokio worker threads with:
- In-flight delegation tasks (worker notification synthesis)
- The event funnel (S2)
- The event sink (S3)

If the pump task doesn't get scheduled within 100ms, it's aborted while holding unread broadcast messages. These messages are lost because the broadcast receiver is dropped.

### Consequence
Late `AgentNotification` events (e.g., `TurnComplete`, tool calls from the brain's final turn) may be lost. The lineage projection sees `BrainRetired` but may miss the terminal notifications that bracket the last turn.

---

## FINDING 7: Ad-hoc Shutdown Ordering Inconsistency (LOW)

### Principle: Teardown Order Must Respect Dependency Direction
The brain connection is the client of the MCP server. The MCP server depends on the delegation channel. Dependencies should be torn down in reverse order.

### Evidence
```rust
// orchestrator.rs:949-952 (run_adhoc)
let _ = connection.shutdown().await;   // 1. brain connection
delegation_handle.abort();              // 2. delegation handler
abort_mcp_handle(mcp_handle).await;     // 3. MCP server
```

Correct dependency chain: Brain → MCP Server → Delegation Handler → Workers
Correct teardown order: Workers → Delegation Handler → MCP Server → Brain

Actual order: Brain killed BEFORE workers and delegation handler. This means:
- If the brain's shutdown triggers any MCP tool calls (unlikely, but possible in future transports), the MCP server is still running but the brain can't receive responses.
- More importantly, it's inconsistent with `run_interactive` end-of-session cleanup, which does: delegation → pump → MCP → brain shutdown.

---

# ROUND 3: Verification — Invariant Cross-Check

| Invariant | Source | Status |
|---|---|---|
| "Must be aborted on session retire — otherwise the listener keeps its port open" | `BrainSession.mcp_handle` doc | ✓ Port closes on abort |
| "Must be aborted whenever the session is retired — otherwise a pump subscribed against the reused connection keeps emitting events tagged with this (now-stale) spur_session_id" | `BrainSession.notification_pump_handle` doc | ⚠ 100ms race, but generally OK |
| "Every terminal emits DelegationCompleted" | `finalize()` doc + code | ✓ Upheld via `finalize()` |
| "The brain MUST deduplicate signals by signal_id across polls" | AGENTS.md signal conventions | N/A (not delegation path) |
| "INV-6: register a cancellation token BEFORE spawning so cancel() arriving between dispatch and spawn still works" | orchestrator.rs:3010 | ✓ Upheld for explicit cancel |
| "INV-6: race execute_delegation against the per-delegation cancellation token" | orchestrator.rs:3076 | ✗ **No implicit shutdown token** |
| "Worker session for the *next* attempt... so the Retry arm can announce" | `run_one_worker_attempt` doc | ✓ Upheld |

---

# Root Cause Summary

The fundamental architectural gap is that **worker tasks are fire-and-forget children of the `handle_delegations` loop, but there is no parent→child cancellation propagation mechanism.** The system assumes that aborting the parent task will implicitly clean up children, which is false in tokio's task model.

This compounds with the **MCP server's task tracker never being closed**, creating a reference leak that prevents detached completion collectors from terminating.

The result is a **cascading reliability failure**:
1. Brain retires/shuts down.
2. `delegation_handle.abort()` fires.
3. Parent loop stops, but children survive.
4. `abort_mcp_handle()` aborts HTTP listener.
5. MCP server's `task_tracker` never closed.
6. Result collectors in task tracker hold `Arc<McpCallbackServer>` alive.
7. Workers complete, but `respond_to.send()` fails (brain gone).
8. OR: Workers hang on `connection.shutdown()`, blocking forever.
9. Brain agent does not receive continuation, stalls.
10. User observes "worker did not shut down" or "shutdown event did not emit."

---

# Recommendations (Ranked by Impact/Effort)

## R1: Propagate Brain Shutdown to Workers (High Impact, Medium Effort)
**Problem**: No implicit cancellation on brain retire.
**Fix**: Before aborting `delegation_handle`, iterate all registered cancellation tokens and cancel them. Requires `CancellationControl` to expose an `active_ids()` method or a `cancel_all()` method.

```rust
// In retire_active_brain and run_adhoc/run_interactive cleanup:
self.cancellation_control.cancel_all().await;
// THEN:
delegation_handle.abort();
```

This gives each in-flight worker a `cancel_token.cancelled()` signal, causing it to exit via the existing `select!` arm and emit `DelegationCompleted(Cancelled)`.

## R2: Close MCP Server Task Tracker on Shutdown (High Impact, Low Effort)
**Problem**: `McpCallbackServer::shutdown()` never called.
**Fix**: Change `abort_mcp_handle` to call `mcp_server.shutdown()` before aborting, or hold the `Arc<McpCallbackServer>` in `BrainSession` and call shutdown explicitly.

```rust
// In BrainSession, store Arc<McpCallbackServer> instead of just the handle.
// In retire_active_brain:
if let Some(server) = b.mcp_server {
    tokio::time::timeout(Duration::from_secs(5), server.shutdown()).await.ok();
}
abort_mcp_handle(b.mcp_handle).await;
```

## R3: Timeout Worker Connection Shutdown (Medium Impact, Low Effort)
**Problem**: `connection.shutdown().await` can hang.
**Fix**: Wrap in `tokio::time::timeout`.

```rust
if tokio::time::timeout(Duration::from_secs(5), connection.shutdown()).await.is_err() {
    tracing::warn!("Worker connection shutdown timed out; forcing drop");
}
// Drop connection regardless to release subprocess handles.
drop(connection);
```

## R4: Use TaskTracker or JoinSet for Worker Tasks (Medium Impact, High Effort)
**Problem**: `tokio::spawn` worker tasks are detached.
**Fix**: Replace `tokio::spawn` inside `handle_delegations` with a `tokio_util::task::TaskTracker` or `tokio::task::JoinSet`. Close the tracker before exiting `handle_delegations` and wait for all workers to finish (with a deadline).

```rust
let worker_tracker = TaskTracker::new();
while let Some(request) = channel.request_rx.recv().await {
    worker_tracker.spawn(async move { ... });
}
// When channel closes (delegation_tx dropped):
worker_tracker.close();
tokio::time::timeout(Duration::from_secs(30), worker_tracker.wait()).await.ok();
```

This ensures worker tasks are given a bounded grace period to clean up before the parent exits.

## R5: Emit DelegationAbandoned on respond_to Failure (Low Impact, Low Effort)
**Problem**: Silent result drop when brain is gone.
**Fix**: In the `respond_to.send()` error path, emit an explicit event.

```rust
if let Err(returned_result) = respond_to.send(result) {
    funnel.emit(SpurEventBody::DelegationAbandoned {
        worker_session: SessionId(request_id),
        reason: "brain disconnected before result delivery".into(),
    });
    // ... existing cleanup_cancelled_review ...
}
```

## R6: Extend Notification Pump Grace Window (Low Impact, Low Effort)
**Problem**: 100ms may be too short under load.
**Fix**: Increase to 500ms or make it configurable.

---

# Appendix: Event Sequence Diagrams

## Current: Brain Retire with In-Flight Worker
```
Brain                    Orchestrator                Worker (tokio::spawn)
 |                          |                              |
 |-- User: new session -->  |                              |
 |                          |-- retire_active_brain()      |
 |                          |   delegation_handle.abort()  |
 |                          |   (parent task cancelled)    |
 |                          |                              |-- (UNAFFECTED: continues)
 |                          |-- abort_mcp_handle()         |
 |                          |   (HTTP task aborted)        |
 |                          |                              |-- worker completes
 |                          |                              |-- respond_to.send() → ERR
 |                          |                              |-- (result dropped)
 |                          |-- new brain spawns           |
 |-- "where is my worker?"  |                              |
```

## Desired: Brain Retire with In-Flight Worker
```
Brain                    Orchestrator                Worker (tokio::spawn)
 |                          |                              |
 |-- User: new session -->  |                              |
 |                          |-- cancellation_control       |
 |                          |   .cancel_all()              |
 |                          |                              |-- cancel_token fires
 |                          |                              |-- exits via select!
 |                          |                              |-- DelegationCompleted(Cancelled)
 |                          |-- delegation_handle.abort()  |
 |                          |-- mcp_server.shutdown()      |
 |                          |   (task_tracker closed)      |
 |                          |-- abort_mcp_handle()         |
 |                          |-- new brain spawns           |
```
