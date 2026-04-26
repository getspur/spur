# bd-arch.23 design synthesis — Alt G (cancellable acquire + heartbeat watchdog, default-off)

After L9 sequential-thinking MCTS over the three design reviews, the chosen design is **Alt G** = sub-problem A (cancellable `semaphore.acquire()`) + sub-problem B2 (heartbeat watchdog, default-off until v1 emitter exists), with a typed `DelegationAbortReason` separate from `CancellationToken` and per-watchdog broadcast subscription.

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Scope: A + B2 | yes | yes | yes | **A + B2** | converged |
| Defer C (bounded request channel) | yes | yes | yes | **defer** | converged |
| Reject B1/B4 (uniform outer timeout) | yes | yes | yes | **reject** | converged |
| Watchdog default-off until heartbeat emitter lands | yes (BLOCKER) | yes (BLOCKER) | yes (BLOCKER) | **default-off** | converged (3-of-3 BLOCKER) |
| Map watchdog firings to `DelegationStatus::Timeout`, not `Cancelled` | yes (BLOCKER) | yes (SHOULD-DO) | yes (BLOCKER) | **Timeout** | converged |
| Don't make `CancellationToken` carry a reason | yes | yes | yes (BLOCKER) | **separate reason channel** | converged |
| Reason mechanism shape | separate `oneshot::Receiver<()>` for watchdog | separate token/bool | typed `DelegationAbortReason` enum + `Arc<Mutex<Option<...>>>` | **codex's typed enum** | follow codex (most general; future-extensible to Stage-2 reasons) |
| Initial grace longer than steady-state | yes | yes (60s) | yes (`max(configured, steady×2)`) | **60s default + max() formula** | merged |
| CPU-burning hang acceptable known-limitation | yes | yes | yes | **accept** | converged |
| **Q2 watchdog mechanism** | shared-map (single subscriber) | per-watchdog broadcast | per-watchdog broadcast | **per-watchdog broadcast** | follow codex+kimi (2-of-3) |
| Heartbeat timeout default | 300s | 90s | "ground in cadence × 3" | **90s** | follow kimi (10s × 9 grounded in stream-backbone spec §733) |
| Test count | 5 | 6 | 7 | **8 (union)** | merged |

## Override rationale

### Q2 watchdog mechanism: per-watchdog broadcast wins 2-of-3

Gemini argued for a shared-map (`DashMap<executor_id, Instant>` updated by a single subscriber, watchdogs poll the map) on the basis of an O(N²) thundering-herd concern. Codex and kimi both argued for per-watchdog broadcast subscription:

- **Existing pattern**: SPUR's `Orchestrator::subscribe`, `EventSink`, and TUI all consume the broadcast directly. Introducing a central liveness service is a new pattern that bd-arch.23 doesn't need.
- **Scale**: default `max_concurrent=5`. 5 subscribers × O(events) per receive ≈ 50 wakeups/sec under typical load. Trivial cost.
- **Cleanup**: per-watchdog subscription dies naturally when the delegation finishes. Shared-map needs explicit eviction logic.
- **Stage-2 doors are open**: if `max_concurrent` grows past ~50 and broadcast pressure becomes real, a shared-map can be introduced as a follow-up — bd-arch.23 doesn't paint Stage-2 into a corner.

Operational cover (kimi): log `RecvError::Lagged` at `WARN` so operators can detect if pressure ever materializes.

### `DelegationAbortReason` enum (codex over kimi/gemini's bool/oneshot)

Kimi suggested a separate token/bool. Gemini suggested a separate `oneshot::Receiver` arm. Codex proposed a typed enum:

```rust
pub enum DelegationAbortReason {
    BrainRequested { reason: String },
    WorkerHeartbeatTimeout { executor_id: String },
    // Stage-2: ResourceLimitExceeded, SandboxTerminated, ...
}
```

Codex's shape wins because:
1. **Typed**: the existing `cancel_token.cancelled()` arm reads the reason and maps to `DelegationStatus`. Type-safe match prevents reason/status drift.
2. **Stage-2 forward-compat**: future cgroup/memory monitors can `request_abort(DelegationAbortReason::ResourceLimitExceeded { ... })` and reuse the existing permit-release path.
3. **Single signaling primitive**: `Arc<tokio::sync::Mutex<Option<DelegationAbortReason>>>` colocated with `CancellationToken`. Helper `request_abort(reason)` sets-if-empty + cancels.

The default (when `cancel_token.cancelled()` fires but no reason was set — e.g., legacy callers) maps to `BrainRequested { reason: "brain requested cancel" }` for back-compat with the existing log line, and emits a `WARN` because that path indicates a caller bypassed `request_abort`.

## The Alt G design

### Sub-problem A: cancellable permit acquire

Replace `let _permit = match semaphore.acquire().await { ... }` at `crates/spur-core/src/orchestrator.rs:3546` with:

```rust
let _permit = tokio::select! {
    biased;
    _ = cancel_token.cancelled() => {
        // Reason was set by whoever called request_abort (brain or watchdog).
        // For pre-acquire cancellation, this is BrainRequested by definition
        // (the watchdog only spawns once execute_delegation starts).
        let status = status_from_abort_reason(&abort_reason).await;
        funnel.emit(SpurEventBody::DelegationCompleted {
            worker_session: spur_acp::types::SessionId(request_id.clone()),
            status: status.clone(),
        });
        let _ = respond_to.send(DelegationResult { status, /* …blank fields… */ });
        cancellation_control_for_task.remove(&request_id).await;
        guard.disarmed = true;
        return;
    }
    permit = semaphore.acquire() => match permit {
        Ok(permit) => permit,
        Err(_) => {
            error!("Semaphore closed — aborting delegation");
            cancellation_control_for_task.remove(&request_id).await;
            return;
        }
    },
};
```

The `biased;` ordering matches the existing convention at `:3601`. The `DelegationGuard` ownership stays correct because the cancel arm explicitly disarms the guard before emitting `DelegationCompleted`.

### Sub-problem B2: heartbeat watchdog

#### Config additions in `crates/spur-acp/src/config/mod.rs` (`WorktreeConfig`)

```rust
#[serde(default = "default_worker_heartbeat_watchdog_enabled")]
pub worker_heartbeat_watchdog_enabled: bool,        // default false

#[serde(default = "default_worker_heartbeat_timeout_secs")]
pub worker_heartbeat_timeout_secs: u64,             // default 90

#[serde(default = "default_worker_heartbeat_initial_grace_secs")]
pub worker_heartbeat_initial_grace_secs: u64,       // default 60
```

#### Abort-reason primitive (new module or in `orchestrator.rs`)

```rust
#[derive(Debug, Clone)]
pub enum DelegationAbortReason {
    BrainRequested { reason: String },
    WorkerHeartbeatTimeout { executor_id: String, idle_for_secs: u64 },
}

#[derive(Clone)]
pub struct DelegationAbortHandle {
    token: tokio_util::sync::CancellationToken,
    reason: Arc<tokio::sync::Mutex<Option<DelegationAbortReason>>>,
}

impl DelegationAbortHandle {
    pub fn new(token: CancellationToken) -> Self { ... }

    pub async fn request_abort(&self, reason: DelegationAbortReason) {
        let mut guard = self.reason.lock().await;
        if guard.is_none() {
            *guard = Some(reason);
            self.token.cancel();
        }
        // first-writer-wins; subsequent abort calls are observed but ignored
    }

    pub async fn observed_reason(&self) -> Option<DelegationAbortReason> {
        self.reason.lock().await.clone()
    }
}

async fn status_from_abort_reason(handle: &DelegationAbortHandle) -> DelegationStatus {
    match handle.observed_reason().await {
        Some(DelegationAbortReason::BrainRequested { reason }) =>
            DelegationStatus::Cancelled { reason },
        Some(DelegationAbortReason::WorkerHeartbeatTimeout { executor_id, idle_for_secs }) =>
            DelegationStatus::Timeout, // worker-hang
        None => {
            tracing::warn!(
                "cancel_token cancelled without DelegationAbortReason — caller bypassed request_abort"
            );
            DelegationStatus::Cancelled { reason: "brain requested cancel".into() }
        }
    }
}
```

#### Watchdog task

Spawned per-delegation (alongside the main `execute_delegation` work) only when `config.worktree.worker_heartbeat_watchdog_enabled = true`.

```rust
async fn run_heartbeat_watchdog(
    request_id: String,
    abort_handle: DelegationAbortHandle,
    mut event_rx: tokio::sync::broadcast::Receiver<SpurEvent>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    timeout_secs: u64,
    initial_grace_secs: u64,
) {
    use tokio::time::{Duration, Instant};

    let steady_timeout = Duration::from_secs(timeout_secs);
    let initial_grace = Duration::from_secs(initial_grace_secs.max(timeout_secs * 2));
    let mut deadline = Instant::now() + initial_grace;
    let mut executor_id: Option<String> = None;

    loop {
        tokio::select! {
            biased;

            _ = &mut stop_rx => {
                // Normal completion path: main task signaled stop.
                return;
            }

            recv = event_rx.recv() => {
                match recv {
                    Ok(event) => {
                        match &event.body {
                            SpurEventBody::DelegationDispatched { request_id: rid, executor_id: eid, .. }
                                if rid == &request_id =>
                            {
                                executor_id = Some(eid.clone());
                                // After dispatch, switch to steady-state timer
                                deadline = Instant::now() + steady_timeout;
                            }
                            SpurEventBody::WorkerHeartbeat { executor_id: eid, .. }
                                if executor_id.as_deref() == Some(eid.as_str()) =>
                            {
                                deadline = Instant::now() + steady_timeout;
                            }
                            _ => {} // not for us; ignore
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            request_id = %request_id,
                            lagged = n,
                            "heartbeat watchdog: lagged broadcast — heartbeats may have been missed; not treating as liveness"
                        );
                        // do NOT reset deadline
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }

            _ = tokio::time::sleep_until(deadline) => {
                let idle = if let Some(eid) = &executor_id {
                    abort_handle.request_abort(
                        DelegationAbortReason::WorkerHeartbeatTimeout {
                            executor_id: eid.clone(),
                            idle_for_secs: timeout_secs,
                        }
                    ).await;
                } else {
                    // No DelegationDispatched seen → still in initial grace.
                    // Treat as a startup hang.
                    abort_handle.request_abort(
                        DelegationAbortReason::WorkerHeartbeatTimeout {
                            executor_id: "<not-dispatched>".into(),
                            idle_for_secs: initial_grace_secs,
                        }
                    ).await;
                };
                return;
            }
        }
    }
}
```

#### Spawn point

Inside the per-delegation `tokio::spawn` at `orchestrator.rs:3537`, after the cancellable permit acquire (so that watchdog only runs when the delegation has actually started):

```rust
let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
let watchdog_handle = if self.config.worktree.worker_heartbeat_watchdog_enabled {
    let event_rx = event_tx.subscribe();
    let watchdog = tokio::spawn(run_heartbeat_watchdog(
        request_id.clone(),
        abort_handle.clone(),
        event_rx,
        stop_rx,
        self.config.worktree.worker_heartbeat_timeout_secs,
        self.config.worktree.worker_heartbeat_initial_grace_secs,
    ));
    Some((watchdog, stop_tx))
} else {
    drop(stop_tx); // immediately drops stop_rx, no watchdog
    None
};
```

After `execute_delegation` returns (success, failure, or cancellation), the main task drops `stop_tx`, which causes the watchdog's `&mut stop_rx` arm to fire and exit cleanly. No abort needed.

### Tests (in `crates/spur-core/tests/delegation_watchdog.rs` — NEW file)

8 tests:

1. **`cancel_during_permit_wait_short_circuits`** — flood semaphore; queue delegation; cancel before permit acquired; assert `DelegationCompleted(Cancelled)` without acquiring permit.
2. **`watchdog_disabled_by_default`** — default config; spawn delegation; verify no watchdog task created (no spurious cancellation in 200ms).
3. **`silent_worker_triggers_watchdog_timeout`** — enabled config; spawn delegation; emit `DelegationDispatched` but NO `WorkerHeartbeat`; advance time past timeout; assert `DelegationStatus::Timeout`.
4. **`heartbeating_worker_survives_indefinitely`** — enabled config; emit periodic heartbeats; advance time past N×timeout; assert no abort.
5. **`initial_grace_period_covers_startup`** — enabled config; no `DelegationDispatched` and no heartbeat; advance time PAST steady-timeout but BEFORE initial_grace; assert no abort yet.
6. **`brain_cancel_preempts_watchdog`** — enabled config; spawn delegation; brain cancels via `request_abort(BrainRequested)` before watchdog timeout; assert `DelegationStatus::Cancelled` (not Timeout).
7. **`lagged_broadcast_does_not_reset_deadline`** — enabled config; saturate broadcast to force `Lagged`; assert deadline NOT reset; eventually times out.
8. **`normal_completion_stops_watchdog_cleanly`** — enabled config; let `execute_delegation` return; drop `stop_tx`; assert watchdog `is_finished()` shortly after.

### CHANGELOG entries

Under `### Fixed`:

```markdown
- **Architecture Risk #23 (semaphore indefinite wait).** Permit acquire is now
  cancellable: `cancel_delegation` arriving while a task is queued for a
  permit short-circuits immediately without acquiring. A heartbeat-based
  watchdog (default-off) detects silent worker hangs and releases the held
  permit after `worker_heartbeat_timeout_secs` (default 90s, configurable).
  Watchdog is gated behind `worker_heartbeat_watchdog_enabled` (default
  `false`) until a v1 `_spur/heartbeat` emitter lands; operators may opt
  in early if their workers emit heartbeats. Watchdog firings map to
  `DelegationStatus::Timeout`, preserving the `Timeout` (worker-hang)
  vs `TimedOut` (review-gate) semantic split. Brain-initiated cancellations
  continue to map to `DelegationStatus::Cancelled`. (bd-arch.23)
```

Under `### Added`:

```markdown
- **Worker heartbeat watchdog configuration.** New `[worktree]` config keys:
  `worker_heartbeat_watchdog_enabled` (bool, default `false`),
  `worker_heartbeat_timeout_secs` (u64, default `90`),
  `worker_heartbeat_initial_grace_secs` (u64, default `60`). See
  `docs/architecture.md` Risk #23 for operational guidance and the no-runtime-toggle
  rollback constraint. (bd-arch.23)
- **`DelegationAbortReason` enum** distinguishing `BrainRequested` from
  `WorkerHeartbeatTimeout`. Stage-2 will extend with `ResourceLimitExceeded`
  / `SandboxTerminated` for cgroup-based termination. (bd-arch.23)
```

## What this fixes

1. **Architecture Risk #23 (cancel UX bug)**: `cancel_delegation` now honored even while a task is queued for a permit.
2. **Architecture Risk #23 (silent-hang starvation)**: a hung worker that stops emitting heartbeats releases its permit within `worker_heartbeat_timeout_secs`, restoring concurrency-pool capacity. Gated behind opt-in flag until heartbeat emitter exists.
3. **Architecture Risk #6 (partial)**: the watchdog `tokio::spawn` is paired with a `oneshot` stop channel, so the watchdog dies with its delegation — no fire-and-forget orphan.
4. **Stage-2 forward compat**: `DelegationAbortReason` is the single typed primitive future cgroup/memory/sandbox monitors will reuse.

## What this does NOT fix

- **CPU-burning hangs** (workers that emit heartbeats but make no progress). Acknowledged limitation. Mitigations deferred: progress-milestone watchdog, per-delegation wall-clock cap chosen by brain (B3), Stage-2 cgroup-based supervision.
- **Dispatch flood** (sub-problem C). Brain-side `SendError::Full` handling is a separate behavior contract; no production evidence dispatch flood is real today.
- **Heartbeat emitter**. bd-arch.23 wires the consumer; the producer is still missing in v1. Default-off prevents this from being a self-own.

## Patch estimate

| Component | LoC | Files |
|---|---|---|
| Sub-problem A: cancellable `select!` around `semaphore.acquire()` | ~15 | `orchestrator.rs` |
| `DelegationAbortReason` enum + `DelegationAbortHandle` helper | ~50 | `orchestrator.rs` (or new `delegation_abort.rs` module) |
| `status_from_abort_reason` helper + integration into existing `select!` at `:3601` | ~25 | `orchestrator.rs` |
| `run_heartbeat_watchdog` task + spawn point + stop_tx plumbing | ~80 | `orchestrator.rs` |
| Config additions to `WorktreeConfig` + serde defaults | ~25 | `spur-acp/src/config/mod.rs` |
| Tests (8 cases) | ~250 | `spur-core/tests/delegation_watchdog.rs` (new file) |
| CHANGELOG entries | ~20 | `CHANGELOG.md` |
| **Total** | **~465 LoC net** (fits codex's high-end estimate, since we're including the abort-reason helper) | |

Risk: **medium**. Sub-problem A is purely additive. Sub-problem B2 is gated behind opt-in flag (default false), so existing deployments see no behavioral change. The risk is in opt-in deployments — a misconfigured timeout or premature flag-flip could fire the watchdog spuriously. Mitigations: (a) default-off, (b) generous defaults (90s timeout, 60s initial grace), (c) test #4 (heartbeating-survives) and #5 (initial-grace) prevent the obvious regressions, (d) `DelegationAbortReason` cleanly distinguishes watchdog firings in telemetry.

## Followups (NOT bd-arch.23 scope)

| Item | Tracking |
|---|---|
| **Implement worker `_spur/heartbeat` emitter** | Required prerequisite before flipping watchdog default to `true`. Either skill-bundle instructions for LLM workers, or server-side synthesis from `AgentNotification` traffic. |
| **Default flip to `worker_heartbeat_watchdog_enabled = true`** | Follow-up after emitter lands + production soak validates 60s initial grace per kimi SHOULD-DO #2. |
| **Sub-problem C (bounded request channel)** | Defer; no observed dispatch flood. |
| **Sub-problem B3 (brain-supplied per-delegation timeout)** | NICE-TO-HAVE follow-up; cleaner long-term design. |
| **Progress-milestone watchdog (CPU-burning hang)** | Defer; instrument before Stage-2. |
| **Stage-2 `DelegationAbortReason` extensions** (`ResourceLimitExceeded`, `SandboxTerminated`) | Stage-2 sandbox/cgroup work. |
| **Initial grace measurement (kimi SHOULD-DO #2)** | Telemetry experiment before default-on flip. |
| **Shared-map watchdog architecture** | Stage-2 if `max_concurrent` grows past ~50 and broadcast pressure becomes real. |
