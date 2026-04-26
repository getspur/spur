# bd-cpf.7 Operational Review — Kimi

**Commit under review:** `1171365` — additive peer-mailbox observability events (`WorkerPeerMessageDrainStarted`, `WorkerPeerMessageDrainTimedOut`, `WorkerPeerMessageLateAckDropped`, `WorkerPeerMessageAckReceived`).
**Reviewer:** kimi  
**Date:** 2026-04-26  
**Scope:** All four candidates; provisional scope from first-principles framing is `DrainStarted` + `DrainTimedOut`. This review evaluates whether that scope holds operationally.

---

## Verdict

**Approve `DrainStarted` + `DrainTimedOut` for bd-cpf.7. Defer `LateAckDropped` and `AckReceived`.**

`DrainStarted` + `DrainTimedOut` are cheap-additive, wire-safe, and close a real Stage-2 observability gap. `LateAckDropped` is blocked on evidence the late-ack path is reachable (Q4 analysis shows it is not today). `AckReceived` is high-volume debug noise with no dashboard consumer.

---

## Pager-risk classification

| Candidate | Pager risk | Rationale |
|---|---|---|
| `WorkerPeerMessageDrainStarted` | **Low** | One event per worker prompt; no per-message dimension; no state mutation. |
| `WorkerPeerMessageDrainTimedOut` | **Low** | Higher volume than `DrainCappedOut`, but same wire path; additive only. |
| `WorkerPeerMessageLateAckDropped` | **N/A (defer)** | Would require behavioral change to keep `ack_rx` alive past drain exit; not additive observability. |
| `WorkerPeerMessageAckReceived` | **N/A (defer)** | Per-ack event volume scales as `O(acks × drains × workers)`; no alerting consumer identified. |

Overall ticket risk: **Low** — purely additive events, `#[non_exhaustive]` enum, no lineage mutation.

---

## Recommendation: scope, thresholds, dashboards

### In scope for bd-cpf.7

1. **`WorkerPeerMessageDrainStarted`**
   - **Fields:** `brain_session_id`, `target_delegation_id`, `candidates_at_start: u32`, `cap_ms: u64`, `quiet_window_ms: u64`
   - **Dashboards:** Peer-mailbox drain latency (pairs with `DrainCappedOut` / `DrainTimedOut`); drain saturation (candidates_at_start histogram); worker prompt health overview.
   - **Alert thresholds-of-interest:** Not directly pageable. Use as denominator for "drains that cap out" and "drains that time out with remaining messages" ratios.

2. **`WorkerPeerMessageDrainTimedOut`**
   - **Fields:** same payload as `DrainCappedOut` (`acks_received`, `remaining_messages`, `cap_ms`, `actual_elapsed_ms`) plus `quiet_window_ms: u64` for symmetry and independent computation.
   - **Dashboards:** Drain health ratio panel (`DrainTimedOut` vs `DrainCappedOut` vs clean completion); remaining_messages histogram at quiet-window exit; actual_elapsed_ms vs cap_ms scatter.
   - **Alert thresholds-of-interest:**
     - `remaining_messages > 0` AND `actual_elapsed_ms >= cap_ms * 0.9` — drain is nearly capped and still has unacked messages. Stage-2 page candidate.
     - `remaining_messages > 0` rate per delegation_id > N/hour — worker is not acknowledging peer messages. Stage-2 alert candidate.

### Deferred

3. **`WorkerPeerMessageLateAckDropped`** — defer to post-Stage-2 persistent-ledger work, or drop entirely if Q4 evidence holds.
4. **`WorkerPeerMessageAckReceived`** — defer indefinitely; add only if a concrete on-call debugging workflow demands per-ack granularity.

### Patch-size estimate for preferred scope

- `SpurEventBody` variants: ~20 LoC in `events.rs`
- Round-trip serde tests: ~40 LoC in `events.rs` (following `drain_capped_out_round_trips` pattern)
- Orchestrator emission (`DrainStarted` at drain entry, `DrainTimedOut` at position [C]): ~10 LoC in `orchestrator.rs`
- Functional drain tests: ~30 LoC in `orchestrator.rs` `peer_mailbox_drain_tests`
- `lineage/projection.rs` wildcard arm: ~1 LoC (add new variants to existing no-op match arm)
- CHANGELOG entry: ~5 LoC

**Total: ~100–110 LoC across 3 files + 1 test module.**

---

## Direct answers to design questions (Q1–Q7)

### Q1. Scope: which candidates ship?

**Ship `DrainStarted` + `DrainTimedOut`. Defer `LateAckDropped` and `AckReceived`.**

- `DrainStarted` is the cheap correlate for every drain. Without it, dashboards must infer "drain began" from the preceding `PromptDispatched` or `DelegationCompleted`, which is imprecise when multiple drains share a `delegation_id` (retry loops, Stage-2 multi-drain).
- `DrainTimedOut` closes the silent-exit problem: today quiet-window timeout with `remaining_messages > 0` is observable only by counting `WorkerPeerMessageIgnored { reason: "drain_timeout" }` per-message events. Aggregating those is strictly more work and loses the `actual_elapsed_ms` / `acks_received` context that `DrainCappedOut` already provides.
- `LateAckDropped` requires either keeping `ack_rx` alive past `drain_peer_acks_with_timeout` return or adding a `Drop` counter to the channel pair. Both are behavioral changes masquerading as observability. The framing doc correctly flags this; my Q4 analysis below confirms the sender is already dropped before drain returns, so the late-ack path is unreachable without code changes.
- `AckReceived` would fire at position [A] for every ack. Volume = `acks_received` across all drains. In the test `drain_resets_quiet_window_on_each_ack`, 4 acks are emitted in one drain. At scale this is `O(acks × drains × workers)` — the highest volume of any candidate. No dashboard or alert today consumes per-ack data. Reject until a concrete consumer is specified.

### Q2. `DrainTimedOut` field shape

**Full payload, symmetric with `DrainCappedOut`, plus `quiet_window_ms`.**

```rust
WorkerPeerMessageDrainTimedOut {
    brain_session_id: String,
    target_delegation_id: DelegationId,
    acks_received: u32,
    remaining_messages: u32,
    cap_ms: u64,
    quiet_window_ms: u64,
    actual_elapsed_ms: u64,
}
```

Symmetry arguments:
- Dashboards can reuse the same panel query for `DrainCappedOut` and `DrainTimedOut` (same field names, same semantics).
- `cap_ms` is meaningful even on quiet-window exit: it tells the operator how much headroom remained before the absolute cap.
- `quiet_window_ms` is required to compute "was this a genuine quiet-window exit or did the cap deadline race with the quiet deadline?" (`actual_elapsed_ms < cap_ms` implies quiet-window fired first).

### Q3. `DrainStarted` field shape

**The provisional shape is correct. No additions needed.**

```rust
WorkerPeerMessageDrainStarted {
    brain_session_id: String,
    target_delegation_id: DelegationId,
    candidates_at_start: u32,
    cap_ms: u64,
    quiet_window_ms: u64,
}
```

`candidates_at_start` is the critical independent variable for drain saturation. `cap_ms` and `quiet_window_ms` capture the tunable limits in effect at drain start (limits may change between deployments; capturing them per-drain avoids join lookups).

No need for `acks_expected` — we do not know how many acks are in flight until they arrive.

### Q4. `LateAckDropped` lifetime question

**The late-ack path is unreachable today. Defer `LateAckDropped`.**

Evidence from `crates/spur-core/src/orchestrator.rs`:

1. `ack_tx` / `ack_rx` are created inside `run_one_worker_attempt` at line 3751:
   ```rust
   let (ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();
   ```
2. `ack_tx` is explicitly dropped at line 3905 **before** `drain_peer_acks_with_timeout` is called:
   ```rust
   drop(ack_tx);
   ```
3. `ack_rx` is moved into `drain_peer_acks_with_timeout` at line 3917.
4. When `drain_peer_acks_with_timeout` returns, `ack_rx` (owned by the function parameter) is dropped.

At this point both halves of the channel are dropped. No sender exists anywhere in the process that could enqueue a late ack. The `_spur/peer_message_consumed` and `_spur/peer_message_ignored` notifications are routed through `spur_ext_interp.rs`, not through this channel — the channel is strictly for the internal "ack received" signal between the ext-notification handler and the drain loop.

**Conclusion:** `LateAckDropped` would be observability for a path that cannot fire. If future Stage-2 refactoring restructures ack routing so the sender outlives the drain, revisit this candidate. Until then, defer.

### Q5. `DrainTimedOut` emission policy: unconditional or `remaining_messages > 0` only?

**Emit unconditionally on quiet-window exit.**

Arguments for unconditional:
1. **Symmetry with `DrainCappedOut`:** `DrainCappedOut` fires regardless of `remaining_messages` (it can be zero if the cap deadline races with the last ack). If `DrainTimedOut` is conditional, dashboards must union two event types with different predicates to compute "total drains observed."
2. **Clean pair for alerting:** An unconditional `DrainTimedOut` plus unconditional `DrainCappedOut` lets operators write:
   - `rate(DrainStarted) - rate(DrainTimedOut) - rate(DrainCappedOut)` = drains that exited via `Ok(None)` (channel closed, clean completion with zero pending).
   - `rate(DrainTimedOut { remaining_messages > 0 })` = drains that went quiet with outstanding work.
3. **Volume is acceptable:** In Stage-1, most drains have `candidates_at_start = 0` and exit via quiet window. The unconditional event doubles the per-prompt event count (one `DrainStarted`, one `DrainTimedOut`), but the event has no per-message dimension and the funnel is unbounded. See Q6.

**Counter-argument (noted but rejected):** Unconditional emission increases volume for drains that completed cleanly. However, the cost of one small JSON event per prompt is negligible compared to the cost of a missed alert because an operator forgot to handle the `remaining_messages == 0` case in a conditional query.

### Q6. Volume control / sampling

**No sampling knob exists in `event_funnel` today. Accept the volume; instrument before Stage-2 if necessary.**

The current `event_funnel` (`crates/spur-core/src/event_funnel.rs`) is a straight-through unbounded mpsc → broadcast channel. There is no sampling, aggregation, or drop logic. Adding such a knob is out of scope for bd-cpf.7.

Volume math for `DrainStarted` + `DrainTimedOut` (unconditional):
- One worker prompt ≈ 1 `DrainStarted` + 1 `DrainTimedOut` = 2 events.
- At 100 worker prompts/minute = 200 events/minute = ~300K events/day.
- Event size: ~200 bytes JSON each = ~60 MB/day.
- This is small compared to `WorkerNotification` stream volume (which carries full content chunks).

**Recommendation:** Ship without sampling. If Stage-2 replay-flood scenarios push event volume past funnel backpressure thresholds, add a `FunnelHandle::emit_sampled(body, rate)` helper in a follow-up ticket. Do not block bd-cpf.7 on infrastructure that does not yet exist.

### Q7. Tests

**Minimum coverage per new variant:**

1. **Wire round-trip (serde):** One test per variant in `crates/spur-acp/src/domain/events.rs` following the `worker_peer_message_drain_capped_out_round_trips` pattern. Assert every field round-trips.
2. **Deserialize-with-missing-fields (replay compat):** For `DrainTimedOut`, verify that a JSON payload missing `quiet_window_ms` deserializes with a sensible default (e.g., `0` via `#[serde(default)]`). This matches the bd-cpf.3/4/5 precedent for forward replay.
3. **Functional test — `DrainStarted` emitted at drain entry:** In `crates/spur-core/src/orchestrator.rs` `peer_mailbox_drain_tests`, add an assert that `spawn_drain` produces exactly one `DrainStarted` with correct `candidates_at_start`.
4. **Functional test — `DrainTimedOut` on quiet-window exit with remaining messages:** Extend `drain_completes_after_quiet_window_with_no_acks` or add a new test that delivers a message, starts drain, advances time past quiet window, and asserts `DrainTimedOut` with `remaining_messages == 1` and `acks_received == 0`.
5. **Functional test — `DrainTimedOut` NOT emitted on cap-hit exit:** Verify that a cap-hit drain produces `DrainCappedOut` but NOT `DrainTimedOut` (mutual exclusivity).
6. **Lineage projection passthrough:** Add new variants to the existing no-op match arm in `lineage/projection.rs` and verify via existing lineage tests that the projection does not panic (it won't, because the arm is a wildcard).

---

## Classification summary

| Candidate | Verdict | Classification | Pager risk |
|---|---|---|---|
| `WorkerPeerMessageDrainStarted` | Ship in bd-cpf.7 | **SHOULD-DO** | Low |
| `WorkerPeerMessageDrainTimedOut` | Ship in bd-cpf.7 | **SHOULD-DO** | Low |
| `WorkerPeerMessageLateAckDropped` | Defer | **NICE-TO-HAVE** (blocked on reachable-path evidence) | N/A |
| `WorkerPeerMessageAckReceived` | Defer indefinitely | **NICE-TO-HAVE** (blocked on consumer demand) | N/A |
| CHANGELOG entry for new events | Add under `## Unreleased` → `### Added` | **SHOULD-DO** | N/A |
| Wire-compat forward-replay test | Deserialize missing `quiet_window_ms` | **SHOULD-DO** | N/A |

---

## CHANGELOG strategy

Add to `CHANGELOG.md` under `## Unreleased` → `### Added`:

```markdown
- **Peer mailbox drain lifecycle events.** `WorkerPeerMessageDrainStarted`
  and `WorkerPeerMessageDrainTimedOut` provide symmetric observability for
  the post-prompt ack drain. `DrainStarted` carries the candidate-set size
  and limits in effect; `DrainTimedOut` mirrors the existing
  `WorkerPeerMessageDrainCappedOut` payload plus `quiet_window_ms`. These
  events are diagnostic-only and do not mutate lineage state. Dashboards
  should migrate drain-health panels from inferred `PromptDispatched`
  timestamps to the explicit `DrainStarted` → `DrainTimedOut`/`DrainCappedOut`
  pair for accurate latency measurement. (bd-cpf.7)
```

Why a tracking note matters: bd-cpf.7 adds wire variants that Stage-2 dashboards will depend on. Future operators auditing event availability need to know when `DrainTimedOut` was introduced. The CHANGELOG is the source of truth for "what event exists in what release."

---

## Summary

`DrainStarted` + `DrainTimedOut` close the most important Stage-2 observability gap (silent quiet-window drain exit) at minimal cost (~100 LoC, no behavioral change, wire-safe). `LateAckDropped` is deferred because the late-ack path is unreachable with current channel lifetimes. `AckReceived` is deferred because it is high-volume debug noise without a consumer. Unconditional emission of `DrainTimedOut` is the correct policy: it creates a clean alerting pair with `DrainCappedOut` and avoids query-complexity pitfalls. No sampling knob is needed today; instrument later if volume becomes material.
