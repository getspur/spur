# bd-arch.23 Operational Review — Kimi

**Commit:** `cecc508c docs(design): bd-arch.23 first-principles framing — semaphore indefinite wait`  
**Scope:** `crates/spur-core` (`orchestrator.rs`, `event_funnel.rs`), `crates/spur-acp` (`config/mod.rs`, `domain/delegation.rs`)  
**Author:** Operational review authored by Kimi (this document).  
**Date:** 2026-04-26

---

## Verdict

**LGTM-with-SHOULD-DOs**

The first-principles framing is sound and the Recommended scope (A + B2) is the right operational trade-off. Sub-problem A is a pure correctness win with near-zero risk. Sub-problem B2 (heartbeat watchdog) is the only fix that addresses the production-starvation failure mode without recreating the `Timeout` vs `TimedOut` semantics bug that killed the previous uniform outer timeout.

Two SHOULD-DOs block a "clean" verdict: (1) the watchdog must be gated behind a config knob defaulting to `false` until a v1 worker heartbeat emitter exists, and (2) the initial grace period must be sized against measured agent cold-start latency, not guessed.

---

## Pager-Risk Classification

| Path | Risk | Rationale |
|---|---|---|
| **Today (no fix)** | **High** | `max_concurrent=5` default. One silent worker hang (network-pinned future, unbounded ACP request, infinite loop) permanently degrades pool capacity by 1. Five hangs = total pool freeze. No automatic detection, no automatic recovery. Brain continues dispatching; 995+ delegations silently buffer in unbounded mpsc. Operator must restart SPUR process. This is a page-worthy outage. |
| **After A only (cancellable acquire)** | **High** | Cancel-during-acquire improves UX but does not change the hung-worker starvation vector. A worker that never cancels itself still holds its permit forever. Pool freeze remains reachable. |
| **After A + B2 (heartbeat watchdog, default-off)** | **Near-zero** | Config `worker_heartbeat_watchdog_enabled = false` (recommended default until heartbeat emitter lands). No new tasks spawned, no new timeout behavior. Identical to "today" but with cancel-during-acquire fixed. |
| **After A + B2 (heartbeat watchdog, default-on)** | **Low–Medium** | Once enabled and a heartbeat emitter exists: silent hangs are detected and cancelled within minutes. Risk shifts to spurious cancellation from misconfigured timeout or missing initial-grace-period slack. CPU-burning hangs (heartbeating but not making progress) still slip through — acknowledged limitation. |
| **After A + B2 + C (bounded request channel)** | **Low–Medium** | `SendError::Full` surfaces dispatch backpressure to the brain. New failure mode for brain: must decide retry/drop/bubble-up. Without brain-side handling, this is a novel crash/loop vector. Defer until dispatch floods are observed. |

---

## Recommendation: Scope, Defaults, Alert Thresholds

### Scope: A + B2 (Recommended), defer C

| Sub-problem | Verdict | Reason |
|---|---|---|
| **A — Cancellable permit acquire** | **Must include** | ~10 LoC, fixes a real UX bug, zero regression risk. |
| **B2 — Heartbeat watchdog** | **Must include** | Only targeted fix for the starvation failure mode that avoids the previous outer-timeout bug. |
| **C — Bounded request channel** | **Defer** | Adds `SendError::Full` brain-side complexity for a problem not yet observed. Dispatch floods are theoretical with current brain loop behavior (1–10 delegations per turn). Revisit if metric shows sustained queue growth. |
| **B4 — Absolute upper bound** | **Reject** | Recreates the exact conditions that broke `Timeout` vs `TimedOut` semantics. Framing doc is correct to exclude this. |
| **B3 — Brain-supplied per-delegation timeout** | **NICE-TO-HAVE follow-up** | Cleaner long-term design, but not required to close Risk #23. |

### Defaults

| Knob | Proposed Default | Rationale |
|---|---|---|
| `worker_heartbeat_watchdog_enabled` | **`false`** | No v1 worker agent currently emits `_spur/heartbeat`. Enabling the watchdog today would fire on every delegation immediately. Flip to `true` only after a worker-side heartbeat emitter lands (or a server-side sidecar in the MCP callback server, per stream-backbone spec §733). |
| `worker_heartbeat_timeout_secs` | **90** | Spec intuition is 10s heartbeat cadence (stream-backbone spec §733). 90s = 9× cadence, giving headroom for jitter, GC pauses, and bursty event loops. Under load, a 10s heartbeat may slip to 15–20s; 90s avoids spurious fire. Detection within ~1.5 minutes is acceptable for a failure mode that previously lasted forever. |
| `worker_heartbeat_initial_grace_secs` | **60** | Worker spawn path: `snapshot_brain_state` → `create_worktree` → `build_connection` → `initialize` → `new_session_with_bypass` → first prompt send → first heartbeat emission. Agent cold-start (e.g., Claude Code, Kimi CLI) can take 5–15s for process spawn + ACP handshake. First prompt round-trip before heartbeat emission adds another 5–10s. 60s clears the startup window with margin. If measured telemetry shows lower, tighten to 45s. |

### Alert Thresholds (for opted-in deployments)

| Metric | Threshold | Severity | Operator Action |
|---|---|---|---|
| `DelegationStatus::Timeout` rate (watchdog-fired) | `> 2 / hour` | **Page** | Indicates silent-hang pattern. Check worker logs for network stalls, unbounded loops, or ACP transport deadlocks. |
| `DelegationStatus::Timeout` rate | `> 0 / 24 hours` | **Ticket** | Any watchdog activation in a 24h window should be investigated in next stand-up, even if within threshold. Zero is the healthy target. |
| `DelegationStatus::Cancelled` rate (brain-initiated) | Spike > 3× baseline | **Warn** | Distinguish from watchdog cancellations. Brain-initiated cancels are normal; sudden spikes may indicate brain-side panic/restart loops. |
| Active delegation count vs `max_concurrent` | `== max_concurrent` for > 5 min | **Warn** | Pool saturation is not itself an error, but sustained saturation without throughput suggests hidden hangs. Correlate with `Timeout` rate. |
| Request channel depth (if C is ever implemented) | `> max_concurrent × 2` for > 1 min | **Warn** | Brain dispatching faster than workers can drain. |

---

## Blockers — NONE

No BLOCKER items. The framing correctly rejects B1/B4 and does not propose any change that would recreate the `Timeout` vs `TimedOut` bug.

---

## SHOULD-DO

### SHOULD-DO #1: Config-gate the watchdog with default `false`

**Rationale:** No production agent emits `_spur/heartbeat` today (stream-backbone plan §1468: "None in v1. Wire format ready; no current agent emits it."). A watchdog with no heartbeat source is an immediate self-own: every delegation would be cancelled after the initial grace period.

**Implementation:** Add `worker_heartbeat_watchdog_enabled: bool` to `WorktreeConfig`, default `false`. The orchestrator only spawns the watchdog task when the flag is `true` AND the worker signals heartbeat capability (or unconditionally once the heartbeat emitter is known to exist).

**Rollback:** When default flips to `true`, operators who see spurious cancellations set `worker_heartbeat_watchdog_enabled = false` and restart SPUR. Document this in CHANGELOG.

### SHOULD-DO #2: Measure actual agent cold-start latency before finalizing `initial_grace_secs`

**Rationale:** The 60s default is an educated guess based on `run_one_worker_attempt` code inspection (snapshot → worktree → connection init → ACP initialize → session → prompt). Real agent startup varies by transport (stdio vs SSE), agent binary size, and host I/O load.

**Implementation:** Before the default-on flip, add a temporary metric/logging that records `Instant::now()` delta from `DelegationRequested` to first `WorkerHeartbeat`. Run across representative agents (Claude Code, Kimi CLI, Codex CLI) for at least 20 cold starts each. Set `initial_grace_secs` to `P99 + 50%`.

### SHOULD-DO #3: Distinguish watchdog timeout from brain-initiated cancel in telemetry

**Rationale:** Today, `cancel_token.cancelled()` always maps to `DelegationStatus::Cancelled { reason: "brain requested cancel" }`. The watchdog firing into the same token would emit the same status with the same reason string, making it impossible to distinguish "brain changed its mind" from "worker was hung" in dashboards.

**Implementation:** Extend the cancel path so the watchdog can inject a different reason. Two options:
- Add a `WatchdogTimeout` variant to cancel semantics (new atomic bool or separate token), mapping to `DelegationStatus::Timeout`.
- Keep `Cancelled` but change reason to `"heartbeat watchdog: no heartbeat for Ns"`.

The architecture doc explicitly wants `Timeout` (worker-hang) for this case. Map watchdog firings to `DelegationStatus::Timeout` to preserve the semantic split.

### SHOULD-DO #4: Add `worker_heartbeat_timeout_secs` and `worker_heartbeat_initial_grace_secs` to config schema with serde defaults

**Rationale:** Hard-coding these values prevents operators from tuning per-environment. A development workstation with slow Docker agents needs different slack than a fast cloud VM.

**Implementation:** Add to `WorktreeConfig`:
```rust
#[serde(default = "default_worker_heartbeat_watchdog_enabled")]
pub worker_heartbeat_watchdog_enabled: bool,
#[serde(default = "default_worker_heartbeat_timeout_secs")]
pub worker_heartbeat_timeout_secs: u64,
#[serde(default = "default_worker_heartbeat_initial_grace_secs")]
pub worker_heartbeat_initial_grace_secs: u64,
```

---

## NICE-TO-HAVE

| Item | Note |
|---|---|
| **Shared-map watchdog vs broadcast-subscription watchdog** | The framing doc raises two architectures. Per-watchdog broadcast subscription is simpler and acceptable for `max_concurrent=5`. A shared-map approach reduces broadcast pressure if `max_concurrent` grows to 50+ in future tiers. Not a blocker for v1. |
| **CPU-burning hang mitigation** | Progress-event-based liveness (no `WorkerProgress` milestone for N minutes) could catch heartbeating-but-stuck workers. Out of scope for bd-arch.23; track as follow-up if default-on telemetry shows this pattern. |
| **Per-agent worker_timeout (B3)** | Cleaner than a global heartbeat timeout because the brain knows expected task duration. Defer to MCP API vNext. |

---

## Operational Questions — Direct Answers

### Q1. Scope: A+B2 (Recommended), A+B2+C, or A only? Defend.

**RECOMMENDED: A + B2.**

- **A only** fixes cancel UX but leaves the High pager risk untouched. Not sufficient.
- **A + B2 + C** adds bounded request channel complexity (`SendError::Full` brain handling) for a dispatch-flood problem that is theoretical today. The brain's natural dispatch rate (1–10 per turn) does not stress the unbounded mpsc. C is defense-in-depth, not a production requirement.
- **A + B2** closes Risk #23's starvation hazard with minimal surface area. The heartbeat watchdog is targeted (only fires on genuinely silent workers), preserves `Timeout` vs `TimedOut` semantics, and generalizes to future per-worker resource caps.

### Q2. B2 watchdog mechanism: each-watchdog-subscribes-to-broadcast, or shared-map-updated-by-single-subscriber?

**RECOMMENDATION: each-watchdog-subscribes-to-broadcast for v1.**

**Rationale:** `max_concurrent` default is 5. At most 5 broadcast subscribers. The broadcast buffer is 4096 slots (~2.5s at peak event rate). Even with 5 slow subscribers, `Lagged` drops are acceptable for heartbeat liveness — missing one heartbeat is fine; the next one resets the timer. The shared-map approach introduces new shared-state plumbing (Arc<Mutex<HashMap<executor_id, Instant>>> + a single funnel subscriber task) for marginal gain. If `max_concurrent` grows to 50+ in future tiers, revisit.

**Operational note:** Log `RecvError::Lagged` at `WARN` in the watchdog so operators can detect if broadcast pressure becomes real.

### Q3. Heartbeat timeout default: pick a number grounded on actual cadence × slack-factor.

**RECOMMENDATION: 90 seconds.**

**Grounding:** No production heartbeat cadence exists yet. The stream-backbone spec §733 proposes 10s as an intuition. With `N = 9` (generous slack for jitter, GC, and event-loop stalls):
- 10s × 9 = 90s.
- Detection within 1.5 minutes is a dramatic improvement over "indefinite."
- Under load, heartbeat emission may slip to 15–20s; 90s still gives 4.5× headroom.
- If the emitter lands with a different cadence (e.g., 30s), adjust to `cadence × 3` (90s) or `cadence × 4` (120s).

**Do not set below 60s.** Any value < 60s risks firing during legitimate but slow operations (large file reads, model initialization) before the worker has a chance to emit its next heartbeat.

### Q4. Initial grace period: how long does worker spawn / first prompt take in practice?

**RECOMMENDATION: 60 seconds default; measure before default-on flip.**

**Code-path timing (estimated from `run_one_worker_attempt`):**
| Step | Estimated Latency |
|---|---|
| `snapshot_brain_state` (git branch) | 50–200 ms |
| `create_worktree` (git worktree add) | 500 ms – 3 s (depends on repo size) |
| `build_connection` + agent process spawn | 2–10 s (stdio transport: fork+exec agent binary; SSE: HTTP handshake) |
| `connection.initialize` (ACP Initialize) | 1–3 s |
| `new_session_with_bypass` | 500 ms – 2 s |
| First prompt send + worker begins processing | 500 ms – 2 s |
| First `_spur/heartbeat` emission | 0–10 s after worker begins (depends on emitter cadence) |
| **Total cold-start to first heartbeat** | **~5–30 s typical; up to 45 s on slow hosts** |

60s clears P99 startup with margin. If measured data shows lower, tighten to 45s. Do not go below 30s — Docker-based agents or large repos can spike.

### Q5. Cancellation event semantics: `Timeout` or `Cancelled` when watchdog fires?

**RECOMMENDATION: `DelegationStatus::Timeout`.**

The architecture-doc comment at `orchestrator.rs:3587-3596` establishes the semantic split:
- `Timeout` = worker fault (hang, crash, never responded).
- `TimedOut` = review-gate timeout (nobody reviewed in 30 min).
- `Cancelled` = brain explicitly requested cancellation.

The watchdog firing is a worker fault (the worker went silent). Map to `Timeout`. Extend the cancel path with a separate `WatchdogTimeout` token/bool, or restructure so the watchdog emits `DelegationCompleted { status: Timeout }` directly instead of piggybacking on the cancel token.

**Dashboard implication:** `Timeout` rate becomes the primary signal for "worker health degraded."

### Q6. CPU-burning hang acknowledgment: acceptable known-limitation or needs separate mitigation?

**ACCEPTABLE known-limitation for bd-arch.23.**

The heartbeat watchdog explicitly detects *silent* hangs (network-pinned, infinite sleep, deadlocked await). A worker that is CPU-burning but still emits heartbeats is a *progress* problem, not a *liveness* problem.

Mitigation options (all deferred):
- Progress-milestone watchdog: no `WorkerProgress` event for N minutes.
- Wall-clock execution cap on `execute_delegation` (B4 variant, but only as a 1-hour absolute bound that exceeds `review_timeout`).
- Operator-initiated `cancel_delegation` (already works after fix A).

**Operational cover:** Document this limitation in the architecture doc and CHANGELOG. If default-on telemetry shows heartbeating-but-stuck workers, open a follow-up ticket.

### Q7. Sub-problem C tradeoffs: `SendError::Full` — new failure mode for brain or just observability?

**BRAIN-SIDE NEW FAILURE MODE. Not worth the complexity today.**

Swapping unbounded mpsc for `mpsc::channel(N)` surfaces `SendError::Full` at every `request_tx.send(...)` call site in the brain (or MCP server, depending on where dispatch originates). The brain must decide:
- **Retry with backoff?** Blocks the brain's turn loop.
- **Drop the delegation?** Silent task loss.
- **Bubble up to MCP tool response?** Changes `delegate_to_worker` return type from success to possible `QueueFull` error, requiring brain-side replanning logic.

All three options are new behavioral surfaces that need design, tests, and potentially beads-state-machine changes. For a problem (dispatch flood) that has never been observed, this is premature hardening.

**Operational alternative:** Add an `unbounded_delegation_queue_depth` gauge metric exported from the orchestrator (count active queued tasks waiting for permits). Alert when `depth > max_concurrent × 10` for > 1 minute. This gives observability without new failure modes.

### Q8. Tests: minimum coverage sufficient?

**PARTIAL — needs one additional test.**

The framing doc lists (a) cancel-during-permit-wait, (b) silent-worker watchdog trigger, (c) heartbeating worker survives, (d) initial-grace-period covers startup. These four are necessary and sufficient for the B2 logic.

**Add (e):** watchdog default-off does not spawn watchdog task (verifies SHOULD-DO #1).

**Add (f):** watchdog firing maps to `DelegationStatus::Timeout`, not `Cancelled` (verifies Q5).

Total test cost: ~100 LoC.

### Q9. Stage-2 forward compat: does the watchdog shape generalize?

**YES — with caveats.**

The per-delegation watchdog task pattern generalizes cleanly:
- **Memory cap:** Replace heartbeat subscriber with a periodic RSS poll; cancel when RSS > limit.
- **cgroups:** Same shape — poll cgroup memory.stat, fire on OOM-like growth.
- **Wall-clock cap:** A second `tokio::time::sleep` in the same select! as the heartbeat timer.

The caveat is that each new cap adds a new cancellation reason, so the `DelegationStatus` mapping needs to scale. Consider adding `DelegationStatus::Killed { reason: String }` as a general worker-termination bucket for Stage-2.

---

## CHANGELOG Strategy

### Entry under `### Fixed`

```markdown
- **Architecture Risk #23 (semaphore indefinite wait).** Permit acquire is now
  cancellable (sub-problem A): `cancel_delegation` arriving while a task is
  queued for a permit short-circuits immediately without acquiring. A
  heartbeat-based watchdog (sub-problem B2) detects silent worker hangs and
  releases the held permit after `worker_heartbeat_timeout_secs` (default 90s,
  configurable). Watchdog is gated behind `worker_heartbeat_watchdog_enabled`
  (default `false`) until a v1 heartbeat emitter lands; operators may opt in
  early if their workers emit `_spur/heartbeat`. Watchdog firings map to
  `DelegationStatus::Timeout`, preserving the `Timeout` (worker-hang) vs
  `TimedOut` (review-gate) semantic split. (bd-arch.23)
```

### Entry under `### Added`

```markdown
- **Worker heartbeat watchdog configuration.** New `[worktree]` config keys:
  `worker_heartbeat_watchdog_enabled` (bool, default `false`),
  `worker_heartbeat_timeout_secs` (u64, default `90`),
  `worker_heartbeat_initial_grace_secs` (u64, default `60`).
  See `docs/architecture.md` Risk #23 for operational guidance.
  (bd-arch.23)
```

### Rollback note (inline or under `### Changed`)

```markdown
- **Watchdog opt-out.** If `worker_heartbeat_watchdog_enabled = true` causes
  spurious cancellations, set it to `false` and restart SPUR — runtime toggle
  is not supported. (bd-arch.23)
```

---

## Patch-Size Estimate

| Component | LoC | Files |
|---|---|---|
| A: cancellable permit acquire (`select!` around `semaphore.acquire()`) | ~10 | `orchestrator.rs` |
| B2: watchdog task spawn + funnel subscription + timer reset | ~50 | `orchestrator.rs` |
| Config: `WorktreeConfig` additions + defaults | ~15 | `spur-acp/src/config/mod.rs` |
| Tests (a–f) | ~100 | `spur-core/tests/...` |
| CHANGELOG | ~15 | `CHANGELOG.md` |
| **Total** | **~190 LoC** | |

**Risk: low-medium.** Sub-problem A is purely additive (no existing behavior changes). Sub-problem B2 is additive behind a default-off flag. The only behavioral change for existing deployments is the cancel-during-acquire fix, which is a bugfix.

---

## Summary

| Criterion | Status |
|---|---|
| Framing correctness | PASS |
| Scope recommendation (A + B2) | **ENDORSED** |
| Sub-problem C deferral | **ENDORSED** |
| Config gating (SHOULD-DO #1) | **MUST implement before merge** |
| Initial grace measurement (SHOULD-DO #2) | **MUST implement before default-on flip** |
| Timeout vs Cancelled semantics (SHOULD-DO #3) | **MUST implement before merge** |
| Pager risk today | **High** |
| Pager risk after fix (default-off) | **Near-zero** |
| Pager risk after fix (default-on, post-emitter) | **Low–Medium** |
| Patch size | ~190 LoC |
| Test coverage target | 6/6 cases |

**Recommended action:** Proceed with implementation of A + B2. Merge only after SHOULD-DOs #1 and #3 are addressed. Defer default-on flip until a v1 heartbeat emitter exists and SHOULD-DO #2 measurement validates the 60s initial grace.
