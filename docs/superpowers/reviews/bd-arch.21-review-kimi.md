# bd-arch.21 Operational Review — Kimi

**Commit:** `e5519eb6 feat(spur-core): bd-arch.21 wire peer mailbox into orchestrator boot + brain_session_id refactor`  
**Scope:** `crates/spur-core` (9 files, +561/-140)  
**Author:** Operational review authored by Kimi (this document).  
**Date:** 2026-04-26

---

## Verdict

**LGTM-with-NITs**

The implementation matches the Alt H synthesis spec. Both original BLOCKER concerns (JoinHandle tracking, brain_session_id resolver) are resolved. CHANGELOG entries are verbatim correct. The default-off path is strictly additive with zero pager risk. The default-on path is gated behind explicit opt-in and covered by 5 integration tests that exercise the first production miles, but lacks load/chaos coverage before high-traffic deployment.

---

## Pager-Risk Classification

| Path | Risk | Rationale |
|---|---|---|
| **Default-off** (`peer_mailbox_enabled = false`) | **Near-zero** | No bundle constructed, no reconciler spawned, no emit sites reachable. Existing 62 peer_mailbox tests continue to pass. Confirmed by test `peer_mailbox_enabled_false_silently_drops_notification`. |
| **Default-on** (`peer_mailbox_enabled = true`) | **Low for dev/staging; medium for production without further soak** | The 5 integration tests validate bundle attachment, reconciler lifecycle, stranded-message drain, slot propagation, and abort-on-drop. They do NOT cover: concurrent session swaps under load, mpsc backpressure at high guard-drop rates, or the three startup-reconcile call sites in an actual brain-session lifecycle. Opt-in is safe for internal validation; production-wide flip should wait for soak + chaos coverage. |

---

## Blockers — NONE

All BLOCKER items from the design review are resolved in this commit.

## SHOULD-FIX — NONE

No operational defects found that would block merge or endanger default-off deployments.

## NITs

| File:Line | Item | Severity | Note |
|---|---|---|---|
| `crates/spur-core/src/orchestrator.rs:922` | `peer_mailbox_bundle()` is `pub` but has zero production callers outside tests. | NIT | Justified as a diagnostic surface, but should be documented "for tests and diagnostics" or gain a health-check consumer in a follow-up. |
| `crates/spur-core/src/orchestrator.rs:928` | `peer_mailbox_reconciler_abort_handle()` is `pub` but has zero production callers outside tests. | NIT | Same as above. Currently used only to assert abort in `orchestrator_drop_aborts_reconciler`. A graceful-shutdown RPC could use it, but none exists yet. |
| `crates/spur-core/tests/peer_mailbox_production_wireup.rs` | 5 tests cover happy-path and basic lifecycle, but no test exercises concurrent slot updates + reconciler emits. | NIT | Add a chaos-style test (rapid slot flips + guard drops) before default-on flip. Defer to post-merge validation. |

---

## Operational Questions — Direct Answers

### Q1. Pager risk (default-off): should be near-zero. Confirm.

**CONFIRMED.**

With default config (`peer_mailbox_enabled = false`), `Orchestrator::new` skips the entire conditional block at `orchestrator.rs:861-891`. `peer_mailbox` remains `None`. No ledger is allocated, no reconciler is spawned, no `background_tasks` entry is added, and no emit site in `spur_ext_interp.rs` or the router is reachable because the bundle is never attached.

Test `peer_mailbox_enabled_false_silently_drops_notification` (`peer_mailbox_production_wireup.rs:169`) asserts this directly: `peer_mailbox_bundle()` returns `None`, and no `WorkerPeerMessageAccepted/Rejected/Undeliverable` events appear on the bus.

**Pager risk: zero.**

### Q2. Pager risk (default-on): are the 5 integration tests sufficient to declare opt-in deployments safe?

**NO — not for high-traffic production without additional soak/chaos coverage.**

The 5 tests are sufficient to declare **dev/staging opt-in safe** and verify the first production miles:

1. `peer_mailbox_enabled_true_attaches_bundle_and_spawns_reconciler` — validates `Orchestrator::new` wire-up, bundle attachment, and end-to-end accept emit.
2. `peer_mailbox_enabled_false_silently_drops_notification` — validates default-off isolation.
3. `reconciler_drains_stranded_message` — validates reconciler loop, ledger transition, and resolver emit.
4. `orchestrator_drop_aborts_reconciler` — validates `JoinHandle` abortion via `Orchestrator::drop`.
5. `session_slot_update_propagates_to_reconciler_emit` — validates slot mutation reaches reconciler emit in real time.

**Categories the tests do NOT cover:**
- **Concurrent session swaps:** Rapid `create_brain_session` → `retire_active_brain` → `create_brain_session` with overlapping guard drops. The slot is an `RwLock<Option<String>>`; a test with rapid flips would confirm no torn reads.
- **mpsc backpressure:** High-rate guard drops (e.g., 10k workers failing simultaneously) could fill the unbounded channel. The channel is unbounded by design, but memory growth under this condition is uncharacterized.
- **Startup reconcile in actual lifecycle:** The three `run_startup_reconcile` call sites are present and idempotent, but no integration test exercises them inside `create_brain_session`, `load_brain_session`, or `run_adhoc` with a pre-seeded ledger.
- **`<no-active-session>` fallback:** No test asserts the fallback string never appears in normal operation.

**Recommendation:** Internal opt-in is safe. Production-wide default flip should wait for a soak period + at least one chaos test (rapid slot flips + concurrent guard drops).

### Q3. CHANGELOG accuracy: were both Added and Fixed entries committed correctly?

**YES — both entries are verbatim correct per the synthesis spec.**

Under `### Added` (line 15-24):
- Mentions Stage-1 wire-up, `peer_mailbox_enabled = true` gating, default `false`.
- Includes Risk #22 warning (in-memory ledger does not prune).
- Includes the no-runtime-toggle warning.
- Tagged `(bd-arch.21)`.

Under `### Fixed` (line 58-63):
- Mentions Architecture Risk #21.
- Describes reconciler spawn at boot + abort on shutdown.
- Notes the previous receiver-drop bug and the "inert subsystem" consequence.
- Tagged `(bd-arch.21)`.

### Q4. Rollback story: does the CHANGELOG include the no-runtime-toggle warning?

**YES.**

CHANGELOG line 23-24:
> To disable, set `peer_mailbox_enabled = false` and restart SPUR — runtime toggle is not supported. (bd-arch.21)

This is exactly the warning the synthesis required. Operators who opt in know they must restart to roll back.

### Q5. Slot lifecycle and dashboard accuracy: walk through one full lifecycle.

**VERIFIED — consistent `brain_session_id` across the entire chain.**

#### (a) Accepted/Rejected events from the router

`PeerMailboxRouter::accept_or_reject` (`router.rs:92`) and `record_terminal` (`router.rs:179`) both take `brain_session_id: &str` as an explicit parameter. Every `funnel.emit` inside these methods uses that parameter directly. **Type-system enforced; no global state.**

#### (b) Undeliverable events from the reconciler

`run_reconciler_loop` (`guard.rs:102`) receives `session_slot: Arc<RwLock<Option<String>>>`. On `Changed` transition, it resolves at emit time:
```rust
let brain_session_id = session_slot.read().await.clone()
    .unwrap_or_else(|| "<no-active-session>".into());
```

#### (c) Slot updated BEFORE `run_startup_reconcile` in all three call sites

| Call site | Slot write | `run_startup_reconcile` |
|---|---|---|
| `create_brain_session` | `orchestrator.rs:1193` | `orchestrator.rs:1199` |
| `load_brain_session` | `orchestrator.rs:2587` | `orchestrator.rs:2593` |
| `run_adhoc` | `orchestrator.rs:2857` | `orchestrator.rs:2863` |

In all three sites, the slot write precedes the reconcile call by exactly 6 lines with no await point between them.

#### Full lifecycle walkthrough

1. **Brain session starts.** `create_brain_session` writes `"session-X"` to the slot, then calls `run_startup_reconcile`, which emits `WorkerPeerMailboxReconciled` (if any entries changed) tagged with `"session-X"`.
2. **Worker sends peer message.** `interpret_peer_message` → `router.accept_or_reject("session-X", ...)` → emits `WorkerPeerMessageAccepted { brain_session_id: "session-X", ... }`.
3. **Guard accepted.** `PeerMessageGuard` held by `WorkerAttemptCtx`.
4. **Guard dropped without finalize.** `Drop` enqueues `StrandedMessage` onto the reconciler mpsc.
5. **Reconciler drains.** Reads slot → `"session-X"` → emits `WorkerPeerMessageUndeliverable { brain_session_id: "session-X", ... }`.

**Result:** `"session-X"` is consistent from session start → accept → stranded → undeliverable. Dashboards keyed on `brain_session_id` see the same value throughout.

### Q6. The `"<no-active-session>"` fallback: when could it appear, and what should an operator do?

**Conditions under which it could fire in production:**

1. **Boot-to-first-session gap.** Orchestrator spawns, reconciler starts, slot is `None`. In the narrow window before the first `create_brain_session` / `load_brain_session` / `run_adhoc`, a stranded message arrives. In practice, no workers are dispatched before a session starts, so this should be impossible under normal operation.
2. **Session-retirement race.** A worker's guard drops AFTER the active brain session is retired but BEFORE the next session starts. This is the most plausible production trigger: a slow worker acks (or drops) during session swap.
3. **Bug: missing slot update.** If a new session-start path is added and forgets to write the slot, the reconciler will emit `"<no-active-session>"` until the slot is written.

**Operator action if it appears:**
- Check logs for the message_id and the surrounding session lifecycle (retire → create timestamps).
- If it correlates with a session swap, the gap is expected but narrow. If it happens outside swaps, file a bug: a slot write is missing.
- Alert threshold: >0 occurrences in a 5-minute window warrants investigation; sporadic single occurrences during session swaps are informational.

### Q7. Operational metric/alert recommendations for opted-in deployments.

**Must-monitor:**

1. **`WorkerPeerMessageUndeliverable` rate.** Non-zero sustained rate indicates workers are dropping guards without finalizing (crashes, panics, task aborts). This is the primary signal of peer-message loss.
2. **`WorkerPeerMessageAuditFailed` rate** (from bd-cpf.5b). Non-zero indicates startup reconcile transition failures. Alert on `transition_kind = "reconcile_to_delivered"` or any audit failure.
3. **Ledger entry count growth** (Risk #22 watch). In-memory ledger never prunes. Monitor memory RSS or export a gauge of `ledger.entry_count()`. Growth >10k entries/session suggests a leak or missing terminalization.

**Should-monitor:**

4. **`WorkerPeerMessageRejected` rate by reason.** High `body_size_exceeded` or `not_in_dag` rates indicate worker misconfiguration or stale plan snapshots.
5. **`WorkerPeerMessageMalformed` rate.** Non-zero indicates worker protocol violations (bad schema, missing fields).
6. **Reconciler task liveness.** The reconciler is a `JoinHandle` in `background_tasks`. A health check should verify the handle is not finished. Currently no direct metric; `peer_mailbox_reconciler_abort_handle()` could be polled by a diagnostic endpoint.
7. **Slot gap duration.** Time between slot being `None` and next write. If >5 seconds, investigate session lifecycle ordering.

**Dashboard query example (Prometheus-style):**
```
rate(spur_events_total{event="WorkerPeerMessageUndeliverable"}[5m]) > 0.1
```

### Q8. `peer_mailbox_bundle()` and `peer_mailbox_reconciler_abort_handle()` — justified for production, or test-only?

**JUSTIFIED as production introspection surfaces, but currently test-only consumers.**

Neither helper is called from `spur-tui`, `spur-cli`, or any other production crate. Both are `pub` on `Orchestrator`.

- `peer_mailbox_bundle()` enables diagnostic callers (health checks, admin RPCs, metrics exporters) to inspect the ledger, router limits, and slot state without reaching into private fields.
- `peer_mailbox_reconciler_abort_handle()` enables graceful-shutdown logic or liveness probes to check whether the reconciler is still running.

**NIT:** They should either (a) gain a production caller (e.g., a `/health` endpoint that checks reconciler liveness), or (b) be documented with a comment like `/// For integration tests and diagnostic introspection.` to prevent future reviewers from flagging them as dead code. Not a merge blocker.

---

## Summary

| Criterion | Status |
|---|---|
| Synthesis spec compliance | PASS |
| Original BLOCKER #1 (JoinHandle tracking) | RESOLVED |
| Original BLOCKER #2 (brain_session_id resolver) | RESOLVED |
| CHANGELOG accuracy | PASS |
| Rollback story documented | PASS |
| Slot lifecycle correctness | PASS |
| Default-off pager risk | Near-zero |
| Default-on test coverage | 5/5 pass; needs chaos/soak before wide production flip |
| Match-completeness (codex SHOULD-DO) | PARTIAL — production caller sites do not exhaustively match every `Acceptance`/`RouterError` variant with typed arms; they propagate errors via `?` or log at warn level. This is acceptable for Stage-1 but should be tightened before default-on. |

**Recommended action:** Merge. Follow-up ticket for default-on flip should include at least one concurrent-slot-flip chaos test and a reconciler liveness metric.
