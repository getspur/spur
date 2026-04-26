# bd-arch.23 operational review (kimi)

**Commit:** `b77ba09f feat(spur-core,spur-acp): bd-arch.23 cancellable permit acquire + heartbeat watchdog (default-off)`
**Scope:** 10 files, +608/-24. 8 new tests, all passing.
**Verdict:** LGTM-with-NITs

---

## Pager-risk classification

| Scenario | Risk level | Rationale |
|---|---|---|
| **Default-off (watchdog disabled)** | **Low** | Sub-problem A (cancellable acquire) is active, but it is a bugfix — cancels that previously silently pended now actually cancel. No new failure mode is introduced. |
| **Default-on / opt-in without heartbeat emitter** | **Medium** (sharp edge) | The `max(initial_grace_secs, timeout_secs × 2)` formula yields an effective initial grace of **180 s** with shipped defaults, not 60 s. This is protective. However, there is **no INFO-level log on watchdog spawn** and **no log on timeout firing**, so an operator who opts in without a heartbeat emitter will see unexplained `DelegationStatus::Timeout` events after ~3 minutes with no obvious causal trail. See SHOULD-FIX below. |

---

## SHOULD-FIX

### 1. Add observability on watchdog spawn and timeout firing
**`crates/spur-core/src/delegation_watchdog.rs:44`** (`maybe_spawn_heartbeat_watchdog`) and **`crates/spur-core/src/delegation_watchdog.rs:106`** (`sleep_until(deadline)` arm).

The watchdog task is completely silent on normal paths. An operator who sets `worker_heartbeat_watchdog_enabled = true` without a v1 heartbeat emitter will experience silent `Timeout` statuses after the initial grace expires. The only logs emitted today are:
- `tracing::warn!` when `cancel_token` fires without an abort reason (back-compat bypass path) — line 20.
- `tracing::warn!` on broadcast `Lagged` — line 96.

Neither explains *why* a delegation was aborted by the watchdog. Recommend:
- `tracing::info!` in `maybe_spawn_heartbeat_watchdog` when spawning: `request_id = %request_id, timeout_secs, initial_grace_secs, "heartbeat watchdog spawned"`.
- `tracing::warn!` in the `sleep_until(deadline)` arm before `request_abort`: `request_id = %request_id, executor_id, idle_for_secs, "heartbeat watchdog timeout fired"`.

This makes the opt-in misuse case self-diagnosing.

---

## NITs

### NIT-1. `initial_grace_secs` default is not the effective grace
**`crates/spur-core/src/delegation_watchdog.rs:64`**

The effective initial grace is `initial_grace_secs.max(timeout_secs.saturating_mul(2))`. With shipped defaults (60 s, 90 s) the effective grace is **180 s**, not 60 s. The CHANGELOG and config docstrings say "default 60s", which is accurate for the config key but misleading for the effective value. Consider documenting the `max()` formula in the config docstring or in `docs/architecture.md` Risk #23 so operators don't set 60 s expecting a 60 s grace.

### NIT-2. `Timeout` status drops `executor_id` and `idle_for_secs`
**`crates/spur-core/src/delegation_watchdog.rs:10-18`**

`status_from_abort_reason` maps `WorkerHeartbeatTimeout { executor_id, idle_for_secs }` to the unit variant `DelegationStatus::Timeout`. The rich metadata is lost from the `DelegationCompleted` event (`crates/spur-acp/src/domain/events.rs:583-586`). Dashboards can count `Timeout` events but cannot attribute them to a specific executor or idle duration without correlating against the (unlogged) abort reason. This is acceptable for v1 but should be revisited when the default flips to `true`.

### NIT-3. Cancel-during-acquire behavioral change is technically observable
**`crates/spur-core/src/orchestrator.rs:3561-3585`**

Before this commit, `cancel_delegation` arriving while a task queued for a permit would leave the cancellation token cancelled but the task would still acquire the permit and run. After this commit, the task short-circuits to `DelegationStatus::Cancelled` without acquiring. No production caller should rely on "cancel ignored = run anyway", but if any integration test or out-of-tree consumer asserted on the old behavior, it will break. The test `cancel_during_permit_wait_short_circuits` correctly documents the new contract.

---

## Direct answers to 10 operational questions

### 1. Pager risk (default-off): is cancel-during-acquire a behavioral regression risk?

**No page risk.** It is a bugfix. The old behavior (cancel silently pended, then the task ran anyway after acquiring the permit) violated the contract of `CancellationControl::cancel`. No caller should have relied on "cancel before permit = ignored"; if one did, it was depending on a bug. The new short-circuit path emits `DelegationCompleted(Cancelled)` and disarms the guard correctly (`guard.disarmed = true` at `orchestrator.rs:3582`).

### 2. Pager risk (default-on, no heartbeat emitter): is misuse obvious?

**Not obvious enough.** There is **no log on watchdog spawn** and **no log on timeout firing**. The only signal is a `DelegationCompleted { status: Timeout }` event after the effective initial grace (180 s with defaults). An operator who flips the flag without reading the synthesis doc will not understand why tasks are timing out.

**SHOULD-FIX:** Add `info!` on spawn and `warn!` on timeout (see SHOULD-FIX #1 above).

### 3. CHANGELOG accuracy

**PASS.** Both entries match the synthesis spec verbatim:
- `### Added` — config keys + `DelegationAbortReason` enum.
- `### Fixed` — Architecture Risk #23 with the cancellable acquire, default-off watchdog, and `Timeout` vs `Cancelled` semantic split.

### 4. Rollback story: runtime flip-off requires restart

**Confirmed.** `WorktreeConfig` is loaded once at orchestrator construction and cloned into the delegation handler:
- Loaded at `orchestrator.rs:822` (`config` stored in `Self`).
- Cloned into `handle_delegations` at `orchestrator.rs:1236`, `2638`, `2910`.
- Cloned again into the per-delegation `tokio::spawn` at `orchestrator.rs:3537`.

There is no hot-reload path. The CHANGELOG correctly notes: "no-runtime-toggle rollback constraint."

### 5. SHOULD-DO #1 (default-off)

**PASS.** Verified by `watchdog_disabled_by_default_no_spawn` (`crates/spur-core/tests/delegation_watchdog.rs:73`). `maybe_spawn_heartbeat_watchdog` returns `None` when `config.worker_heartbeat_watchdog_enabled == false`, and the test advances time 10 000 s with no abort.

### 6. SHOULD-DO #2 (initial-grace measurement)

**Deferred entirely.** The 60 s default is the synthesis estimate. No pre-flight telemetry is added in bd-arch.23. The implementation does emit `WorkerHeartbeat` events (`spur-core/src/spur_ext_interp.rs:57`) with `worker_ts: Option<String>`, so a future telemetry analysis can ground the default before the flip. Marking Risk #23 Fixed in architecture docs is correct; the measurement belongs in the follow-up "default flip" ticket.

### 7. SHOULD-DO #3 (Timeout vs Cancelled telemetry)

**PASS.** `status_from_abort_reason` (`delegation_watchdog.rs:10-28`) cleanly maps:
- `BrainRequested` → `DelegationStatus::Cancelled`
- `WorkerHeartbeatTimeout` → `DelegationStatus::Timeout`
- `None` (legacy bypass) → `DelegationStatus::Cancelled` + `WARN` log

The dashboard split (`Timeout` for worker-hang, `TimedOut` for review-gate, `Cancelled` for brain-initiated) is preserved.

### 8. SHOULD-DO #4 (config keys with serde defaults)

**PASS.** Three new keys in `WorktreeConfig` (`crates/spur-acp/src/config/mod.rs:482-532`):
- `worker_heartbeat_watchdog_enabled: bool` with `serde(default = "default_worker_heartbeat_watchdog_enabled")` → `false`
- `worker_heartbeat_timeout_secs: u64` with `serde(default = "default_worker_heartbeat_timeout_secs")` → `90`
- `worker_heartbeat_initial_grace_secs: u64` with `serde(default = "default_worker_heartbeat_initial_grace_secs")` → `60`

Unit test `worktree_defaults_have_heartbeat_watchdog_disabled` (`config/mod.rs:843`) validates all three defaults on both `Default` and parsed-empty-config paths.

### 9. Alert thresholds-of-interest

**No change from earlier proposal.** The implementation does not introduce new alert rules; it provides the signal.

| Signal | Threshold | Action |
|---|---|---|
| `DelegationStatus::Timeout` rate | > 2 / hour | Page |
| `DelegationStatus::Timeout` rate | > 0 / 24 h | Ticket |

These thresholds remain appropriate because `Timeout` should be rare in healthy deployments. The default-off gate means the signal is zero until the operator opts in.

### 10. Test naming + coverage

**PASS.** All 8 test names are operationally clear:

1. `cancel_during_permit_wait_short_circuits` — sub-problem A correctness
2. `watchdog_disabled_by_default_no_spawn` — default-off gate
3. `silent_worker_triggers_watchdog_timeout` — steady-state timeout path
4. `heartbeating_worker_survives_indefinitely` — liveness reset path
5. `initial_grace_period_covers_startup` — startup grace path
6. `brain_cancel_preempts_watchdog` — first-writer-wins / reason precedence
7. `lagged_broadcast_does_not_reset_deadline` — back-pressure safety
8. `normal_completion_stops_watchdog_cleanly` — orphan-free cleanup

Coverage is complete for the stated scope. The one gap not covered (and acknowledged out-of-scope) is CPU-burning hangs — a worker that emits heartbeats but makes no progress will not be caught.

---

## Summary

The implementation correctly realizes the Alt G synthesis. The default-off gate makes this safe to ship. The one operational sharp edge is observability: an operator who opts in without a heartbeat emitter will get silent timeouts. Adding spawn + timeout logs is a small, high-value SHOULD-FIX that can land in a fast-follow commit without blocking the architecture-risk closure.

**Recommended follow-up:**
- Commit the spawn/timeout logging (SHOULD-FIX #1).
- Document the effective `max(initial_grace, timeout×2)` grace in `docs/architecture.md` Risk #23 (NIT-1).
