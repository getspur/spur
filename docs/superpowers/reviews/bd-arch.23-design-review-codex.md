# bd-arch.23 Design Review (Codex)

L3+ Rust-idiom review for Architecture Risk #23: semaphore indefinite wait and concurrency-pool starvation.

## Recommendation

Scope should be **A + B2**, with two guardrails:

1. **A is mandatory and small:** race `semaphore.acquire()` against the existing per-delegation cancellation token. This fixes the queued-cancel bug without touching review-gate timeout semantics.
2. **B2 is the right direction, but only if heartbeat capability is explicit:** current source has `WorkerHeartbeat` wire handling through `_spur/heartbeat`, but no evidence that every worker emits heartbeats or that a production cadence is configured. Do not ship a watchdog that assumes all agents heartbeat by default.

For B2, keep `tokio_util::sync::CancellationToken` as a pure cancellation primitive. Do **not** try to make it carry a reason. Add a separate first-writer-wins abort reason signal, then have the cancellation select arm map `BrainRequested` to `DelegationStatus::Cancelled { .. }` and `WorkerHeartbeatTimeout` to `DelegationStatus::Timeout`.

For the heartbeat source, a **per-active-worker `broadcast::Receiver` is idiomatic enough for the current scale** if the watchdog subscribes before `execute_delegation` starts and handles `Lagged` deliberately. A shared liveness map is more Stage-2-shaped, but it adds global mutable state and cleanup obligations that are not needed to fix bd-arch.23.

## Classifications

**BLOCKER**

- A: `semaphore.acquire().await` must be cancellable.
- B2 must preserve worker `Timeout` versus review `TimedOut`; no uniform outer timeout.
- B2 must not classify watchdog-triggered cancellation as `DelegationStatus::Cancelled`.
- B2 must establish a real heartbeat contract: emitter availability, cadence, initial grace, and opt-in or default behavior for non-heartbeating agents.
- Watchdog lifetime must have a clean stop path on normal delegation completion.

**SHOULD-DO**

- Use a separate abort reason enum/slot beside `CancellationToken`.
- Use `biased;` in the semaphore-acquire `select!`, matching the existing cancellation-first convention around the execute-delegation race.
- Add focused tests for cancel-before-permit, watchdog timeout, heartbeat reset, startup grace, and status classification.
- Treat `broadcast::error::RecvError::Lagged(_)` as a liveness risk, not a heartbeat.

**NICE-TO-HAVE**

- Factor a small `DelegationAbort` helper so future cgroup/memory monitors can reuse the same cancellation path.
- Expose heartbeat timeout and startup grace as config, ideally per agent.
- Consider a shared liveness projection later if watchdogs need more than heartbeat timestamps.

## Q1: Scope

Choose **A + B2**.

A alone fixes the queued-cancel bug but does not address the main starvation risk: a worker holding a permit forever. B2 targets held permits without recreating the previous uniform outer timeout bug documented in `orchestrator.rs`. C is real defense-in-depth, but bounded delegation ingress introduces backpressure semantics that need separate product decisions: retry, fail fast, or surface queue-full to the brain.

The design should explicitly say B2 catches **silent** hangs. A CPU-bound or stuck worker that still emits heartbeat remains out of scope until Stage 2 resource controls or progress-based liveness.

## Q2: Watchdog mechanism

Prefer **per-active-worker broadcast subscription** for this patch.

This is idiomatic in tokio when the source is already an event bus: each consumer owns a `broadcast::Receiver`, loops on `recv()`, and handles `Lagged` and `Closed`. SPUR already uses this pattern for orchestrator subscribers: `Orchestrator::subscribe()` returns `event_tx.subscribe()`, the durable event sink subscribes to the same broadcast, and the TUI event loop handles `Lagged` rather than introducing a shared state layer.

The current `EventFunnel` is an emitter, not a subscription API. `FunnelHandle` only exposes `emit()` and `lineage_snapshot()`. A watchdog that consumes `SpurEvent` should therefore receive a `broadcast::Receiver<SpurEvent>` or a small subscription factory from the orchestrator, not overload `FunnelHandle` with read-side responsibilities.

Per-watchdog subscription has one important shape constraint: heartbeats carry `executor_id`, while cancellation is keyed by delegation `request_id`. Since `execute_delegation` generates the worker `SessionId`, a watchdog spawned at the outer delegation level either needs to learn `request_id -> executor_id` from `DelegationDispatched`, or the spawn point needs to move closer to `run_one_worker_attempt` and receive the cancellation handle. Learning from `DelegationDispatched` is acceptable if the receiver is created before `execute_delegation` starts.

A shared map updated by one subscriber is also valid tokio, but it is not the simpler idiom here. It adds a long-lived task, a `HashMap<executor_id, Instant>`, cleanup on terminal events, and lock/watch coordination. That is worthwhile if Stage 2 needs a central liveness service, but for bd-arch.23 it increases state surface before there is evidence that `max_concurrent`-sized receivers are a bottleneck.

## Q3: Heartbeat Timeout Default

This is not yet grounded enough to pick a hard default.

The source defines and interprets `_spur/heartbeat` into `SpurEventBody::WorkerHeartbeat`, but the repo search did not show a universal worker-side emitter or a configured cadence for worker heartbeats. Older stream-backbone docs mention "one per 10s" as an intuition, not an implementation contract.

The safe design is:

- Add config only after identifying the real emitter path.
- Use `timeout >= cadence * 3` for steady-state slack.
- Use a longer startup grace, because initialization, session creation, and first prompt can all happen before the first worker heartbeat.
- If an agent has no heartbeat capability, disable B2 for that agent or require an explicit opt-in before watchdog enforcement.

## Q4: Initial Grace Period

Yes, use a longer first-heartbeat deadline.

The worker attempt path creates a worktree, initializes the connection, creates a session, emits `WorkerSpawned`, emits `DelegationDispatched`, and then prompts. A watchdog that starts before those steps must not treat startup silence the same as a stalled, already-running worker.

Recommended shape: `initial_grace = max(configured_initial_grace, steady_timeout * 2)` or a separately configured value. Once the first matching heartbeat arrives, switch to the steady-state timeout.

## Q5: Cancellation Reason

Do **not** make `CancellationToken` carry a reason.

`CancellationToken` is idiomatically a wakeup/cancellation primitive. It answers "has cancellation been requested?", not "why?". Encoding reason into or around it by convention makes every caller depend on side effects that the type does not express.

Use a separate first-writer-wins reason slot:

```rust
enum DelegationAbortReason {
    BrainRequested { reason: String },
    WorkerHeartbeatTimeout { executor_id: String },
}
```

Implementation options:

- `Arc<tokio::sync::Mutex<Option<DelegationAbortReason>>>` plus helper `request_abort(reason)` that sets the reason if empty and then calls `token.cancel()`.
- Or a `watch::Sender<Option<DelegationAbortReason>>` if later consumers need to observe reason changes directly.

The select arm should read the reason after `cancel_token.cancelled()` resolves. If no reason is present, default to brain cancellation for backward compatibility, but log because that indicates a caller bypassed the helper.

This lets watchdog timeout return `DelegationStatus::Timeout` while user cancellation still returns `DelegationStatus::Cancelled { reason }`. It preserves the existing public `DelegationStatus` variants and avoids another cross-crate match update.

## Q6: CPU-Burning Hang

B2 does not detect a worker that is unproductive but still emits heartbeats.

That limitation is acceptable for this scope if the design states it plainly. Heartbeat means "the worker sidecar/event path is alive", not "the task is making progress". Detecting alive-but-stuck work needs progress milestones, wall-clock execution budgets chosen by the brain, or Stage 2 process/resource supervision.

## Q7: Sub-Problem C

Defer C.

The unbounded request channel can grow under a runaway brain, but bounding it changes the delegation API's failure modes. The brain must then handle `Full`: retry later, shed work, or convert to an explicit error. That is a separate behavior contract and should not be bundled into the starvation fix.

## Q8: Tests

Minimum tests:

- Cancel while waiting for the semaphore returns `DelegationStatus::Cancelled` without acquiring a permit.
- Watchdog timeout sets abort reason to `WorkerHeartbeatTimeout`, cancels the token, and the main select returns `DelegationStatus::Timeout`.
- Matching heartbeat resets the steady-state deadline.
- Non-matching heartbeat does not reset the deadline.
- Initial grace prevents false timeout before the first heartbeat.
- Normal delegation completion stops the watchdog before it later cancels the token.
- `Lagged` broadcast receives do not count as heartbeats.

If an internal `DelegationAbortReason` enum is introduced, test every match arm in the status mapping helper. If a public `DelegationStatus` variant is introduced instead, update round-trip serialization, clipping, TUI rendering, lineage projection, `should_preserve_worktree`, and `should_commit_worker_diff`.

## Q9: Stage-2 Forward Compatibility

The watchdog is Stage-1 infrastructure, but it can compose with Stage 2 if the cancellation boundary is reason-based rather than heartbeat-specific.

The part tied to in-process tokio assumptions is the heartbeat consumer: it depends on the SPUR event bus and workers emitting `_spur/heartbeat`. That is fine for silent-hang detection inside the current orchestrator.

The future-compatible part should be the abort API:

```rust
request_abort(DelegationAbortReason::WorkerHeartbeatTimeout { ... });
request_abort(DelegationAbortReason::ResourceLimitExceeded { ... });
request_abort(DelegationAbortReason::SandboxTerminated { ... });
```

Stage 2 cgroup or memory monitors can then share the same permit-release path without pretending to be heartbeats. If the patch hard-codes "watchdog timeout means cancel" without a typed reason, Stage 2 will have to unwind that design.

## Suggested `tokio::select!` Shape For A

Use cancellation-first bias, matching the existing execute-delegation select:

```rust
let _permit = tokio::select! {
    biased;
    _ = cancel_token.cancelled() => {
        let status = status_from_abort_reason(&abort_reason).await;
        funnel.emit(SpurEventBody::DelegationCompleted {
            worker_session: spur_acp::types::SessionId(request_id.clone()),
            status: status.clone(),
        });
        let _ = respond_to.send(DelegationResult {
            status,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        });
        cancellation_control_for_task.remove(&request_id).await;
        return;
    }
    permit = semaphore.acquire() => match permit {
        Ok(permit) => permit,
        Err(_) => {
            error!("Semaphore closed - aborting delegation");
            cancellation_control_for_task.remove(&request_id).await;
            return;
        }
    },
};
```

The exact response path should respect `DelegationGuard` ownership so `DelegationCompleted` is emitted exactly once. The important property is that a queued task observes cancellation before waiting forever on the permit.

## Watchdog Lifetime Trace

Recommended lifetime:

1. Create `event_rx = event_tx.subscribe()` before `execute_delegation` starts.
2. Create `stop_tx, stop_rx = oneshot::channel::<()>()`.
3. Spawn watchdog with `request_id`, `cancel_token`, abort-reason handle, `event_rx`, `stop_rx`, initial grace, and steady timeout.
4. Watchdog first waits for `DelegationDispatched { request_id, executor_id }`, with initial grace still running.
5. After it knows `executor_id`, matching `WorkerHeartbeat { executor_id, .. }` resets the steady sleep.
6. On timeout, watchdog first writes `DelegationAbortReason::WorkerHeartbeatTimeout`, then calls `cancel_token.cancel()`, then exits.
7. On normal completion, the main task drops or sends `stop_tx`; watchdog receives close/stop and exits without cancelling.
8. On broadcast `Closed`, watchdog exits; on `Lagged`, it logs and continues without resetting liveness.

Dropping the stop sender on every exit path is enough if the watchdog treats `Err(_)` from the oneshot as stop. If there are multiple early returns, wrap the sender in a small guard or ensure the main async block owns it until after cleanup.

## Patch-Size Estimate

Recommended A + B2 implementation:

- A cancellable permit acquire: 15-25 LoC.
- Abort reason helper and cancellation-control adjustment: 40-70 LoC.
- Watchdog spawn/loop and event subscription plumbing: 80-130 LoC.
- Config for heartbeat timeout/startup grace/capability: 30-60 LoC.
- Tests: 120-180 LoC.

Total: **285-465 LoC**, depending on how much config surface is required. If heartbeat capability is already guaranteed elsewhere, this can shrink to roughly 180-260 LoC.

## Final Call

Proceed with **A + B2**, but make heartbeat enforcement capability-aware and reasoned. The Rust-idiom center of gravity is: cancellation token for wakeup, typed reason beside it, broadcast receiver as a short-lived event consumer, and a oneshot stop signal for watchdog lifetime.
