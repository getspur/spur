# SPUR Critical Findings & Recommendations

> Reviewed 2026-04-16. All findings verified against source code with line references.

## Priority Matrix

| # | Finding | Severity | Blast Radius | Frequency | Fix Effort |
|---|---------|----------|-------------|-----------|------------|
| 1 | Stranded executors — missing `DelegationCompleted` | **CRITICAL** | Session-killing | On shutdown/error | Small |
| 2 | Broadcast event loss — lineage projection drift | **HIGH** | Silent data corruption | Under burst load | Tiny → Medium |
| 3 | Untracked `JoinHandle`s — stale events after shutdown | **HIGH** | State corruption | Every shutdown | Medium |
| 4 | Silent event swallowing — `_ => {}` catch-alls | **HIGH** | Wrong UI state | On reconnect/new variants | Medium |
| 5 | Worktree orphaning — disk space leak | **HIGH** | Resource exhaustion | Every unclean shutdown | Medium |
| 6 | No retry backoff — unbounded cost burn | **HIGH** | Cost explosion | On persistent failures | Small |

---

## Finding 1: Stranded Executors (CRITICAL)

### Problem

Multiple early-exit paths in the delegation dispatch bypass `finalize()`, the only function that emits `DelegationCompleted`. When this happens:

- The lineage projection shows the executor as permanently **"Running"**
- The brain's MCP tool call (`delegate_to_worker`) **hangs forever** — the `oneshot` response channel is dropped
- The entire brain session is **stuck** — it can't proceed without the delegation result

### Evidence

```
orchestrator.rs:2087-2091
    let _permit = match semaphore.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            error!("Semaphore closed — aborting delegation");
            return;  // ← No DelegationCompleted emitted. Brain hangs.
        }
    };
```

Other affected paths: `__pm_*` internal ops that fail, agent-not-found lookups, any future early return added to the delegation body.

### Fix: `DelegationGuard` (RAII)

A guard struct that emits `DelegationCompleted(Failed)` and sends a failure response on Drop, unless explicitly disarmed by `finalize()`.

```rust
struct DelegationGuard {
    funnel: FunnelHandle,
    worker_session: SessionId,
    respond_to: Option<oneshot::Sender<DelegationResult>>,
    disarmed: bool,
}

impl Drop for DelegationGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            self.funnel.emit(SpurEventBody::DelegationCompleted {
                worker_session: self.worker_session.clone(),
                status: DelegationStatus::Failed {
                    error: "delegation aborted (early exit or task cancelled)".into(),
                },
            });
            if let Some(tx) = self.respond_to.take() {
                let _ = tx.send(DelegationResult::failed("delegation aborted"));
            }
        }
    }
}
```

Place at the top of the `tokio::spawn` body at `orchestrator.rs:2085`. Existing `finalize()` sets `guard.disarmed = true`. Every early `return` — including tokio task abort on shutdown — triggers the guard.

**Scope**: ~40 lines in `orchestrator.rs`. No cross-crate changes. Fixes the entire class of bugs, not just one instance.

---

## Finding 2: Broadcast Event Loss (HIGH)

### Problem

The event bus uses `broadcast::channel(4096)`. The TUI drains at `DRAIN_CAP_PER_FRAME = 8` (`app.rs:1497`). At 30fps, the TUI processes **240 events/sec max**.

A worker doing rapid file operations can emit 1000+ events/sec (`WorkerFileTouched`, `WorkerNotification` chunks). When the broadcast buffer fills, `recv()` returns `Lagged(n)` — those `n` events are **permanently lost** for that subscriber.

The lineage projection, which is the TUI's source of truth, misses events. Executor state becomes stale or incorrect.

### Evidence

```
event_funnel.rs:94   — broadcast::channel(4096)
app.rs:1497          — const DRAIN_CAP_PER_FRAME: u32 = 8;
                       // 8 events × 30fps = 240 events/sec max throughput
```

### Fix: Two-phase

**Phase 1 (1 line)**: Increase `DRAIN_CAP_PER_FRAME` from 8 to 64.

```rust
const DRAIN_CAP_PER_FRAME: u32 = 64;  // was 8
```

At 30fps this processes 1920 events/sec. Each `lineage.apply()` is O(1) — 64 calls is ~10μs. No TUI stutter risk.

**Phase 2 (30 lines)**: Detect `Lagged` and flag for re-projection.

```rust
match spur_rx.recv().await {
    Ok(event) => self.handle_spur_event(event),
    Err(broadcast::error::RecvError::Lagged(n)) => {
        tracing::warn!(lost = n, "broadcast lagged — requesting re-projection");
        self.lineage_stale = true;
        // Future: replay from NDJSON log to rebuild lineage
    }
    Err(broadcast::error::RecvError::Closed) => break,
}
```

---

## Finding 3: Untracked JoinHandles (HIGH)

### Problem

Fire-and-forget `tokio::spawn` calls inside the orchestrator create tasks that outlive their logical owner:

| Location | Task | Impact when orphaned |
|---|---|---|
| `orchestrator.rs:1300` | Brain ext-notification pump | Emits stale `AgentExtNotification` after brain is retired |
| `orchestrator.rs:3123` | Worker ext-notification pump | Emits stale `WorkerHeartbeat`/`Progress`/`FileTouched` after executor is done |
| `orchestrator.rs:2085` | Delegation task | On shutdown, `apply_worktree_cleanup` is aborted mid-execution → orphaned worktrees |

### Fix: `TaskTracker`

Use `tokio_util::task::TaskTracker` to track all spawned tasks:

```rust
// In Orchestrator:
task_tracker: tokio_util::task::TaskTracker,

// Replace tokio::spawn with:
self.task_tracker.spawn(async move { ... });

// On shutdown:
self.task_tracker.close();
if tokio::time::timeout(Duration::from_secs(5), self.task_tracker.wait())
    .await
    .is_err()
{
    tracing::warn!("force-aborting {} remaining tasks", self.task_tracker.len());
}
```

**Scope**: Add `tokio-util` dependency, ~20 lines changed. Also fixes Finding 5 (worktree orphaning) — cleanup tasks get a 5s grace period instead of immediate abort.

---

## Finding 4: Silent Event Swallowing (HIGH)

### Problem

`app.rs:582` has a `_ => {}` catch-all after matching only 4 of ~25 `SpurEventBody` variants for brain status tracking. Recently added variants (`BrainReconnecting`, `BrainReconnected`, `BrainReconnectFailed`) are silently dropped.

After a brain reconnect, the status bar shows stale state ("Streaming" when the brain is actually reconnecting).

Any future `SpurEventBody` variant will be silently ignored until someone manually adds a match arm. No compile-time safety net.

### Fix: Explicit arms + `#[non_exhaustive]`

```rust
// In spur-acp/src/domain/events.rs:
#[non_exhaustive]
pub enum SpurEventBody { ... }

// In app.rs, replace _ => {} with explicit arms:
SpurEventBody::BrainReconnecting { .. } => {
    self.brain_status = BrainStatus::Thinking;
}
SpurEventBody::BrainReconnected { .. } => {
    self.brain_status = BrainStatus::Ready;
}
SpurEventBody::BrainReconnectFailed { .. } => {
    self.brain_status = BrainStatus::Error("reconnect failed".into());
}
// ... explicit no-op arms for events that don't affect brain status
```

**Scope**: ~50 lines across `app.rs` + 1 attribute in `events.rs`. Provides compile-time enforcement for all future variants.

---

## Finding 5: Worktree Orphaning (HIGH)

### Problem

`apply_worktree_cleanup` runs inside the delegation task (`orchestrator.rs:2085`). On shutdown, tokio aborts the task, and the cleanup call is interrupted. Git worktrees are left on disk.

No startup cleanup exists — `WorktreeManager::new()` creates an empty `HashMap`.

### Fix: Startup cleanup + detached cleanup

```rust
// In WorktreeManager:
pub async fn cleanup_orphans(&self) -> Result<usize> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&self.repo_root)
        .output().await?;
    // Parse output, remove any worktree with "spur-" prefix not in self.active
    // Returns count of removed worktrees
}
```

Call `cleanup_orphans()` during orchestrator startup, before any delegations are dispatched.

For in-flight cleanup: move `apply_worktree_cleanup` to a detached task that's tracked by the `TaskTracker` (Finding 3 fix), so it gets the 5s grace period.

---

## Finding 6: No Retry Backoff (HIGH)

### Problem

The retry loop in `execute_delegation` re-spawns workers immediately on `ReviewDecision::Retry`. No exponential backoff, no max-retry limit at the orchestrator level.

A brain in a retry loop can burn through API credits with no cooldown.

### Fix: Exponential backoff

```rust
// Inside the retry loop in execute_delegation:
let backoff = Duration::from_secs(2u64.pow(attempt_n.min(6) - 1)); // 1s, 2s, 4s, 8s, 16s, 32s, 64s cap
tokio::time::sleep(backoff).await;

funnel.emit(SpurEventBody::ExecutorRetryBackoff {
    executor_id: executor_id.clone(),
    attempt_n,
    backoff_secs: backoff.as_secs(),
});
```

Add `max_retries` to agent config (default 5). After max retries, emit `DelegationCompleted(Failed { error: "max retries exceeded" })`.

---

## Implementation Order

```
Week 1: Finding 1 (DelegationGuard)     — eliminates session-killing bug
         Finding 2 Phase 1 (drain cap)   — 1-line fix, immediate throughput gain
Week 2: Finding 3 (TaskTracker)          — clean shutdown, fixes Finding 5 too
         Finding 6 (retry backoff)        — cost protection
Week 3: Finding 4 (explicit match arms)  — maintainability + correctness
         Finding 2 Phase 2 (Lagged)       — resilience under extreme burst
         Finding 5 (startup cleanup)      — disk hygiene
```

Findings 1 + 2 Phase 1 are the highest-leverage changes: ~45 lines total, fixing the two most impactful bugs.
