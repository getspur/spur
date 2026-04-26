# bd-arch.23 — semaphore indefinite wait + concurrency-pool starvation: first-principles framing

## What the architecture doc states

From `docs/architecture.md` Risk #23 (severity High, status Open):

> Semaphore indefinite wait — `semaphore.acquire().await` has no timeout. A deadlocked worker holds its permit forever. New delegations queue silently with no queue-depth cap or cancellation. First principles: a bounded resource without timeout or preemption is a permanent starvation hazard. One hung worker blocks the entire concurrency pool.

## What the territory actually shows

Direct code grounding:

1. **Permit acquire is unbounded**. `crates/spur-core/src/orchestrator.rs:3546`:
   ```rust
   let _permit = match semaphore.acquire().await {
       Ok(permit) => permit,
       Err(_) => { /* semaphore closed */ return; }
   };
   ```
   No `tokio::time::timeout`, no `select!` against the cancellation token. A task that has been spawned but is waiting for a permit will wait forever.

2. **Cancellation token is registered but unused for permit-wait**. The cancel_token is created at `:3531` BEFORE `tokio::spawn` (per INV-6 — "register before spawn so cancel arriving between dispatch and spawn still works"). It is plumbed into the spawned task. But it is only raced against `execute_delegation` at `:3601`, NOT against `semaphore.acquire().await` at `:3546`. So a `cancel_delegation` MCP call that arrives while the task is queued for a permit is silently ignored — the cancel_token's `cancelled()` never gets polled until after the permit is held.

3. **Held permits have no maximum hold time**. The `_permit` is held for the lifetime of `execute_delegation`. The comment at `:3584-3596` explicitly removed a previous outer timeout because it broke the `Timeout` (worker-hang) vs `TimedOut` (review-timeout) semantics:
   > A previous hardcoded 300s outer timeout always fired before the 1800s default review timeout, cancelling the delegation mid-`select!`, dropping the ReviewSink entry's receiver without emitting Resolved/TimedOut, and returning `DelegationStatus::Timeout` (worker-hang) to the brain.
   So the design intentionally has no outer timeout. A hung worker holds its permit until process termination.

4. **Request channel has no depth cap**. `DelegationChannel.request_rx` is the receiver side of an unbounded mpsc (per the existing wiring). 1000 delegations dispatched while `max_concurrent=5` puts 995 silently buffered.

5. **`max_concurrent` source**. `WorktreeConfig.max_concurrent` (default 5) is the concurrency limit, possibly overridden by the `MaxConcurrentWorkers` license quota. Three call sites at `orchestrator.rs:1209`, `2606`, `2876` resolve the effective value before passing it to `handle_delegations`.

## The three sub-problems

bd-arch.23 must decide which of these to tackle:

### Sub-problem A — Permit acquire is not cancellable

**Severity in practice:** medium-high. Cancellation arrives via the `cancel_delegation` MCP tool or via a brain reconnect/restart. If a brain (or operator) tries to cancel a queued delegation, today the cancel sits in the cancel_token until the permit is finally acquired — only then does the `select!` at `:3601` notice the cancellation and short-circuit `execute_delegation`. This is a poor UX (cancel appears no-op) but does not corrupt state.

**Fix shape:** wrap `semaphore.acquire()` in `tokio::select!` against `cancel_token.cancelled()`. If cancel wins, emit `DelegationCompleted(Cancelled)` via the guard and return without acquiring.

**Cost:** ~10 LoC.

### Sub-problem B — Held permits have no maximum hold time

**Severity in practice:** high. This is the "deadlocked worker holds its permit forever" failure. A worker that is alive (not crashed) but stuck in an infinite loop, an unbounded ACP request, a network-pinned future, etc. will hold the permit until process restart. With `max_concurrent=5`, five hung workers freeze the entire system.

**Fix shape options:**
- **B1 — Outer worker timeout** (rejected by the existing code comment because it breaks `Timeout` vs `TimedOut` semantics). The comment is correct; recreating this bug is not acceptable.
- **B2 — Heartbeat-based liveness check.** Workers already emit `WorkerHeartbeat` events (per §4 of architecture doc). Add a watchdog task per worker: if no heartbeat for `worker_heartbeat_timeout_secs`, the watchdog cancels the cancel_token, which (after fix A) lets the `select!` at `:3601` short-circuit and return `DelegationStatus::Timeout` (worker-hang) without breaking the review-gate semantics.
- **B3 — External operator timeout.** Provide a per-delegation `worker_timeout: Option<Duration>` config knob exposed via the MCP `delegate_to_worker` API. If absent, no timeout. Brain (which has full domain context) chooses whether to bound a given delegation.
- **B4 — Two-tier timeout.** A long absolute upper bound (e.g., 1 hour) that always applies, plus B3 for shorter brain-supplied bounds.

The existing comment (lines 3584-3596) only forbids B1 because the outer timeout was UNIFORM and shorter than the review timeout. A timeout that is (a) longer than the review_timeout, OR (b) heartbeat-based (so it only fires when the worker is genuinely silent), avoids the previous bug.

**Cost:** B2 ≈ 60 LoC + 1 config knob. B3 ≈ 30 LoC + 1 MCP arg. B4 ≈ 80 LoC.

### Sub-problem C — Request channel has no queue-depth cap

**Severity in practice:** low-medium. The brain typically doesn't dispatch hundreds of delegations at once; the natural flow is 1–10 per turn. But: a misbehaving brain (or runaway brain-loop scenario) could fill the channel with dispatch requests faster than the workers can drain them. Memory growth is bounded by the brain's loop rate, but operators have no visibility.

**Fix shape:** swap unbounded mpsc for `mpsc::channel(N)` with `N = max_concurrent * K`. Brain-side `try_send` returns `Full` when saturated; that surfaces as a typed error to the brain.

**Cost:** ~30 LoC + a config knob for K.

## Reachability today vs after fix

| Scenario | Today | After A only | After A+B2 (heartbeat watchdog) |
|---|---|---|---|
| Healthy delegation pipeline | works | works | works |
| Brain cancels a queued delegation | cancel ignored until permit acquired | cancel honored immediately | cancel honored immediately |
| Worker hangs (silent infinite loop) | permit held forever; pool degraded by 1 per hang; 5 hangs freezes pool | permit held forever (A doesn't touch this) | watchdog detects no heartbeat for `T`s → cancels token → `select!` short-circuits → permit released → `DelegationStatus::Timeout` |
| Worker hangs (CPU-bound but emits heartbeat) | permit held forever | permit held forever | permit held forever (B2 only catches silent hangs; CPU-burning workers still emit heartbeat) |
| Brain dispatches 1000 delegations | 995 sit silently buffered | same | same |
| Permit-pool exhaustion deadlock | not actually a deadlock, just starvation; new requests pile up forever | same | watchdog releases held permits when workers go silent |

## Stage-2 forward compatibility

Risk #6 (general TaskTracker introduction in spur-core) and Risk #8 (no fault isolation between brain and workers) overlap with this work. Specifically, Risk #8 says:
> no outer worker timeout exists (hang = indefinite), no memory limits / cgroups, no sandbox.

bd-arch.23 sub-problem B is exactly the worker-hang piece of Risk #8. The other two (memory limits, sandbox) are out of scope.

The watchdog task (B2) needs a JoinHandle tracked somewhere — either per-delegation (in `tokio::spawn` paired with the worker) or in `background_tasks` (orchestrator-level supervisor). Per-delegation is cleaner because the watchdog dies naturally when the delegation finishes; it doesn't need to outlive the worker.

## Scope question

The framing splits cleanly:

| Scope | Sub-problems | LoC | Risk addressed |
|---|---|---|---|
| **Minimal (A only)** | cancellable permit acquire | ~10 | partial #23 — cancel UX fixed; hung-worker pool starvation NOT fixed |
| **Recommended (A + B2)** | + heartbeat watchdog | ~70 | full #23 — silent-hang detection works; CPU-burning hang still slips through (acknowledged limitation) |
| **Aggressive (A + B2 + C)** | + bounded request channel | ~100 | #23 + soft cap on dispatch flood |
| **Maximal (A + B4 + C)** | + absolute upper bound | ~130 | #23 + worst-case bound on permit hold time |

My provisional recommendation: **A + B2** ("Recommended"). Sub-problem C is a defense-in-depth that hasn't bitten anyone in production; defer to a follow-up if dispatch floods become real. Sub-problem B4's absolute upper bound recreates risk of the previous outer-timeout bug; B2 (heartbeat-based) is the targeted fix.

## Heartbeat-watchdog design (B2 details)

Workers already emit `WorkerHeartbeat` events through the funnel (per §4 of architecture doc). The watchdog needs to:

1. Subscribe to the funnel (via `broadcast::Receiver`).
2. Filter for heartbeats matching the current delegation's `executor_id` / `delegation_id`.
3. Reset a `tokio::time::Sleep` deadline on every matching heartbeat.
4. If the sleep elapses without a reset, call `cancel_token.cancel()`.
5. Exit when the delegation finishes (cancel_token consumed by the main `select!`, OR a separate "watchdog-stop" oneshot).

Open questions:
- **Heartbeat cadence.** What's the current heartbeat interval? Need to set the watchdog timeout to `cadence * N` where N gives slack for jitter. Probably N=3 or N=5.
- **Subscriber lifetime.** Does spawning N watchdogs each subscribing to the broadcast cause `Lagged` events under high event volume? Probably acceptable for `max_concurrent=5` watchdogs but worth noting.
- **Heartbeat rate vs CPU-burning hang.** Workers that are CPU-pinned but emitting heartbeats slip through. This is a real limitation — flag it explicitly and accept it (acknowledged in the doc cost table above).
- **Initial grace period.** First-heartbeat delay can be longer than steady-state cadence (worker startup, ACP handshake). Watchdog should give a longer initial deadline.

Alternative: watchdog could poll the delegation's `last_heartbeat_at` from a shared map updated by the funnel subscriber, rather than each watchdog having its own broadcast subscription. Less event-bus pressure; more shared-state plumbing. Reviewers should weigh.

## Design questions for reviewers

1. **Scope**: A+B2 (Recommended), A+B2+C, or A only? Defend the choice.

2. **B2 watchdog mechanism**: each-watchdog-subscribes-to-broadcast, OR shared-map-updated-by-single-subscriber? Which is more idiomatic and less footgun-prone?

3. **Heartbeat timeout default**: what's the current heartbeat cadence in `worker_notification_pump` (or wherever heartbeats originate)? Need to ground the default on actual cadence × slack-factor.

4. **Initial grace period**: should the watchdog give a longer first-heartbeat deadline (e.g., 2× steady-state) to avoid spurious cancellations during worker startup? Or can the worker-spawn path be considered "alive" until the first prompt is sent?

5. **Cancellation event semantics**: when the watchdog fires, the existing `cancel_token` cascade emits `DelegationStatus::Cancelled`. Per the architecture-doc comment (lines 3584-3596), the spec wants `DelegationStatus::Timeout` (worker-hang) for this case. Do we extend the cancel_token shape with a reason (Cancelled vs WatchdogTimeout), or emit a different event from the watchdog?

6. **CPU-burning hang acknowledgment**: the watchdog cannot detect a CPU-pinned worker that still emits heartbeats. Is this an acceptable known-limitation or does it need a separate mitigation (e.g., progress-event-based liveness, or wall-clock execute_delegation cap)?

7. **Sub-problem C tradeoffs**: bounded mpsc adds a new failure mode (`SendError::Full`). Brain-side handling of `Full` is non-trivial — does it retry, drop, or bubble up? Is C worth the surface complexity for a problem that hasn't been observed?

8. **Tests**: minimum coverage = (a) cancel-during-permit-wait fires `DelegationCompleted(Cancelled)` without acquiring; (b) silent-worker (no heartbeat) triggers watchdog cancel; (c) heartbeating worker survives indefinitely; (d) initial-grace-period covers worker startup. Anything else essential?

9. **Stage-2 forward compat**: does the watchdog shape generalize to per-worker-process resource caps (cgroups, memory) in Stage-2, or is it a Stage-1-only point fix?

## Out of scope

- **Risk #8 sandbox/cgroups/memory limits**: separate ticket.
- **Risk #6 general TaskTracker introduction**: separate ticket; bd-arch.23 only stores at most one new JoinHandle (the per-delegation watchdog) and that lives inside the existing per-delegation tokio::spawn.
- **Sub-problem C if not endorsed**: defer to follow-up.
- **Per-agent worker_timeout config**: brain-supplied via MCP would be cleaner; deferred unless reviewers strongly endorse.

## Cost estimate (Recommended scope = A + B2)

- A: cancellable permit acquire: ~10 LoC.
- B2: heartbeat watchdog spawn + timeout default in config + funnel subscription + per-delegation lifecycle: ~60 LoC.
- Tests: (a)-(d) above: ~80 LoC.
- CHANGELOG entry: ~10 LoC.
- **Total: ~160 LoC.**

Risk: **low-medium**. Sub-problem A is purely additive (cancel is currently silent; making it not silent doesn't break anything). Sub-problem B2 changes timeout behavior — a misconfigured timeout could fire spuriously and look like a regression. Mitigations: (a) generous default, (b) configurable, (c) test coverage for the steady-state heartbeat case.
