# bd-arch.23 Design Review

**Author:** Gemini
**Date:** 2026-04-26
**Target:** bd-arch.23 — semaphore indefinite wait + concurrency-pool starvation

## Executive Summary & Recommendation
* **Scope:** **A + B2** (Cancellable permit acquire + Heartbeat watchdog). Sub-problem C (channel bounds) adds backpressure complexity without proven need, and B4 (absolute timeout) risks regressing the review-gate semantics.
* **Watchdog Mechanism:** Shared-map updated by a single subscriber. Per-watchdog broadcast subscriptions create an $O(N^2)$ thundering herd problem where every heartbeat wakes every watchdog.
* **Heartbeat Default:** Must be opt-in (disabled by default) in v1, as no current worker agent emits `_spur/heartbeat`. Default to 300s (5 minutes) once the MCP callback server emitter is merged.
* **Estimated Patch Size:** ~150-180 LoC.

## Question Resolutions

### Q1. Scope Selection
**Decision:** A + B2.
**Rationale:** Sub-problem A is a strict UX bug-fix (cancellation should not silently pend behind permit acquisition). Sub-problem B2 specifically targets the high-severity worker-hang starvation without bringing back the `Timeout` vs `TimedOut` race condition that absolute timeouts (B1/B4) caused. Sub-problem C introduces `SendError::Full` backpressure which the Brain is not currently equipped to handle elegantly.

### Q2. Watchdog Mechanism
**Decision:** Shared-map updated by a single subscriber.
**Rationale:** `tokio::sync::broadcast` sends every message to every subscriber. If there are $N$ active watchdogs, a single heartbeat from one worker wakes up all $N$ watchdogs, only for $N-1$ of them to discard it. This causes $O(N^2)$ wakeups. A single background supervisor should subscribe to the funnel, update a `DashMap<String, Instant>` (keyed by `executor_id`), and individual watchdogs should simply `tokio::time::sleep` and periodically poll their entry. This prevents broadcast channel lag and decouples the event bus from worker tasks.

### Q3. Heartbeat Cadence & Timeout Default
**Decision:** 300s (5 minutes) default, but **disabled by default in v1**.
**Rationale:** The framing doc anticipates an MCP callback server emitting heartbeats every 10s. A 300s timeout provides a generous 30x slack factor, robust against network jitter. **CRITICAL FINDING:** Because no v1 agent currently emits `_spur/heartbeat` (as noted in `2026-04-14-spurevent-stream-backbone-plan.md`), enabling the watchdog unconditionally today will break all workers. The feature must be driven by an explicit `worker_heartbeat_timeout_secs` config, defaulted to `None` or `0` for now.

### Q4. Initial Grace Period
**Decision:** Implement an extended initial grace period (e.g., 2x steady-state).
**Rationale:** Worker startup (ACP handshakes, LLM context loading, container spin-up) is significantly slower and more variable than steady-state execution. The watchdog should grant an extended deadline before the first heartbeat is received to avoid spurious kills during boot.

### Q5. Cancellation Event Semantics
**Verification:** `cancel_token` is a standard `tokio_util::sync::CancellationToken`. It is created via `CancellationControl::register` at `orchestrator.rs:3539` (before the spawn) and consumed at `orchestrator.rs:3606`. It does not carry a payload or reason field.
**Decision:** Do not reuse `cancel_token` for the watchdog. Use a separate `oneshot::Receiver` (`watchdog_rx`).
**Rationale:** Overloading the `cancel_token` would cause watchdog timeouts to incorrectly emit `DelegationStatus::Cancelled`. To emit `DelegationStatus::Timeout` (worker-hang) correctly, add a new branch to the `tokio::select!` at `orchestrator.rs:3601`:
```rust
let (result, executor_id_opt) = tokio::select! {
    biased;
    _ = cancel_token.cancelled() => { /* Cancelled (user/brain) */ },
    _ = watchdog_rx => { /* Timeout (worker hang) */ },
    // execute_delegation...
};
```
This isolates the state machine paths without modifying the existing `CancellationControl` plumbing.

### Q6. CPU-Burning Hang Acknowledgment
**Decision:** Accept as a known limitation.
**Rationale:** A worker that infinite-loops but still runs a background heartbeat thread will bypass B2. Detecting this requires semantic progress limits (`_spur/progress_milestone`) which is too complex for this point-fix.

### Q7. Sub-problem C Tradeoffs
**Decision:** Defer.
**Rationale:** Bounding the MPSC is trivial, but handling the resulting `SendError::Full` in the Brain is not. Until we have a clear backpressure strategy (e.g., block the Brain's turn loop, drop delegations, or queue locally), this adds unproven surface area.

### Q8. Tests
**Decision:** Essential coverage required.
**Rationale:** 
1. Cancel during permit wait interrupts acquire and fires `DelegationCompleted(Cancelled)` (Sub-problem A).
2. Silent worker triggers `watchdog_rx` and yields `DelegationStatus::Timeout`.
3. Worker emitting heartbeats survives past the timeout.
4. Initial grace period prevents early termination.
5. User cancel preempts a pending watchdog.

### Q9. Stage-2 Forward Compat
**Decision:** The shared-map approach generalizes perfectly.
**Rationale:** A Stage-2 supervisor enforcing cgroups/memory limits will naturally act as a central singleton monitoring workers. The single funnel-subscriber updating a shared map fits this topology perfectly, paving the way for a unified worker-monitor subsystem.

## Classification
* **BLOCKER:** Sub-problem A fix (wrap permit wait in `select!`).
* **BLOCKER:** Separate `watchdog_rx` to preserve `Timeout` vs `Cancelled` semantics (Q5).
* **BLOCKER:** Default watchdog to OFF until a heartbeat emitter exists (Q3).
* **SHOULD-DO:** Single-subscriber shared-map over per-task broadcast receivers (Q2).
* **SHOULD-DO:** Extended initial grace period (Q4).
* **NICE-TO-HAVE:** Sub-problem C channel bounds (Q7).
