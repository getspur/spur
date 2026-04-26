# bd-cpf.7 design synthesis — DrainStarted + DrainTimedOut (additive observability)

After L9 sequential-thinking MCTS over the three design reviews, the chosen scope is:
- **IN**: `WorkerPeerMessageDrainStarted`, `WorkerPeerMessageDrainTimedOut`
- **OUT (defer)**: `WorkerPeerMessageLateAckDropped`, `WorkerPeerMessageAckReceived`

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Ship `DrainStarted` | yes | yes | yes | **yes** | converged |
| Ship `DrainTimedOut` | yes | yes | yes | **yes** | converged |
| Defer `LateAckDropped` | yes | yes | yes | **defer** | converged |
| Defer `AckReceived` | yes | yes | yes | **defer** | converged |
| Q5: emit `DrainTimedOut` only when `remaining_messages > 0` | yes | NO (unconditional) | yes | **conditional** | follow gemini+codex (2-of-3) |
| Q2: include `cap_ms` on `DrainTimedOut` | yes | yes | NO | **include** | follow gemini+kimi (2-of-3) |
| `candidate_set_for_target` helper extraction | implicit | implicit | YES (BLOCKER) | **extract** | follow codex |
| Wire-compat replay test (deserialize missing fields) | yes | yes | yes | **add** | converged |
| CHANGELOG entry under `### Added` | implicit | yes | implicit | **add** | follow kimi |
| Doc-comment parallel to `DrainCappedOut` | (silent) | (silent) | NICE-TO-HAVE | **add** | follow codex (cheap) |
| Future `DrainFinished` for clean denominator | (silent) | (silent) | NICE-TO-HAVE follow-up | **defer (followup)** | follow codex |

## Override rationale

### Q5 (emission policy): conditional wins 2-of-3

Kimi argued for unconditional emission on the basis of alerting algebra: `rate(DrainStarted) - rate(DrainTimedOut) - rate(DrainCappedOut) = clean exits`. Gemini and codex rejected this because:

- **Quiet-window expiry with empty mailbox is the normal flow**, not anomalous. Naming the normal flow "TimedOut" is a misnomer that confuses on-call.
- **Cap-hit is intrinsically anomalous regardless of `remaining_messages`**, which is why `DrainCappedOut` fires unconditionally. The asymmetry is correct.
- **Volume**: every prompt drain typically exits via quiet window with `remaining_messages == 0`. Unconditional `DrainTimedOut` would double per-prompt event volume on the common clean path.

The right shape for the clean denominator is a separate `WorkerPeerMessageDrainFinished` event with explicit `exit_reason`, deferred to a follow-up ticket if dashboards actually need it.

### Q2 (`cap_ms` on `DrainTimedOut`): include wins 2-of-3

Codex argued cap_ms is irrelevant when the cap didn't fire. Gemini and kimi argued for inclusion with a concrete operational case: if `quiet_window_ms >= cap_ms` (misconfig), `actual_elapsed_ms` will be ≈ `cap_ms` and the operator can detect it. Without `cap_ms`, the misconfig is invisible from this event alone.

The dashboard-reuse argument (same panel query for both events) is also concrete operational value. Codex's objection is aesthetic.

## The Alt B design

### New event variants in `crates/spur-acp/src/domain/events.rs`

```rust
/// Diagnostic-only. Emitted at drain entry to anchor latency / saturation
/// dashboards. Does NOT mutate lineage state. Pairs with the eventual
/// drain exit event (`WorkerPeerMessageDrainCappedOut` or
/// `WorkerPeerMessageDrainTimedOut`); a clean exit (quiet window with
/// `remaining_messages == 0`) emits no exit event by design.
WorkerPeerMessageDrainStarted {
    brain_session_id: String,
    target_delegation_id: crate::domain::delegation::DelegationId,
    candidates_at_start: u32,
    cap_ms: u64,
    quiet_window_ms: u64,
},

/// Diagnostic-only. Emitted when the drain exits via quiet-window
/// timeout WITH non-terminal messages still in the mailbox. Does NOT
/// count as message-loss observability — that is per-`WorkerPeerMessageIgnored`.
/// Use this for drain-health dashboards (worker stopped acking but
/// did not consume all peer messages).
///
/// Mutually exclusive with `WorkerPeerMessageDrainCappedOut` for any
/// given drain. Not emitted on the clean-exit path
/// (`remaining_messages == 0`).
WorkerPeerMessageDrainTimedOut {
    brain_session_id: String,
    target_delegation_id: crate::domain::delegation::DelegationId,
    acks_received: u32,
    remaining_messages: u32,
    cap_ms: u64,
    quiet_window_ms: u64,
    actual_elapsed_ms: u64,
},
```

### Drain code change in `crates/spur-core/src/orchestrator.rs`

```rust
async fn drain_peer_acks_with_timeout(...) {
    // 1. Compute candidates BEFORE the loop (so DrainStarted has the
    //    same dedup as the post-loop remaining_messages computation).
    let candidates_at_start = candidate_set_for_target(bundle, delegation_id).await;

    funnel.emit(SpurEventBody::WorkerPeerMessageDrainStarted {
        brain_session_id: brain_session_id.to_string(),
        target_delegation_id: delegation_id.clone(),
        candidates_at_start: candidates_at_start.len() as u32,
        cap_ms: max_total.as_millis() as u64,
        quiet_window_ms: quiet_window.as_millis() as u64,
    });

    // 2. Existing loop unchanged.
    // ...

    // 3. After cap_hit decision, recompute candidate set (state may have advanced).
    let candidates = candidate_set_for_target(bundle, delegation_id).await;
    let remaining_messages = candidates.iter().filter(...).count() as u32;

    if cap_hit {
        funnel.emit(SpurEventBody::WorkerPeerMessageDrainCappedOut { ... });
    } else if remaining_messages > 0 {
        // NEW: emit DrainTimedOut only when quiet-window exit leaves work.
        funnel.emit(SpurEventBody::WorkerPeerMessageDrainTimedOut {
            brain_session_id: brain_session_id.to_string(),
            target_delegation_id: delegation_id.clone(),
            acks_received,
            remaining_messages,
            cap_ms: max_total.as_millis() as u64,
            quiet_window_ms: quiet_window.as_millis() as u64,
            actual_elapsed_ms,
        });
    }

    // 4. Existing record_terminal loop unchanged.
}

/// Helper: compute the deduplicated candidate set for a target delegation.
/// Used at drain entry (for `candidates_at_start`) and at drain exit
/// (for `remaining_messages`) so the two metrics cannot drift.
async fn candidate_set_for_target(
    bundle: &PeerMailboxBundle,
    delegation_id: &DelegationId,
) -> Vec<LedgerEntry> {
    let mut candidates = bundle.ledger.pending_for_target(delegation_id).await;
    candidates.extend(
        bundle.ledger.non_terminal_entries().await
            .into_iter()
            .filter(|entry| &entry.envelope.target_delegation_id == delegation_id),
    );
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|entry| seen.insert(entry.envelope.message_id));
    candidates
}
```

### Tests

In `crates/spur-acp/src/domain/events.rs` `worker_peer_event_tests` mod:
- Round-trip serde for `WorkerPeerMessageDrainStarted` (assert all 5 fields round-trip).
- Round-trip serde for `WorkerPeerMessageDrainTimedOut` (assert all 7 fields round-trip).
- Forward-replay deserialize: a JSON missing `quiet_window_ms` should NOT silently default — these are required fields on a brand-new variant. (Following codex's discipline: `#[serde(default)]` is for fields *added to existing variants*, not for new variants.)

In `crates/spur-core/src/orchestrator.rs` drain tests (or peer mailbox e2e):
- `DrainStarted` is emitted exactly once at drain entry with correct `candidates_at_start`.
- Quiet-window exit with `remaining_messages > 0` → exactly one `DrainTimedOut`.
- Quiet-window exit with `remaining_messages == 0` → zero `DrainTimedOut`.
- Cap-hit exit → exactly one `DrainCappedOut`, zero `DrainTimedOut` (mutual exclusivity).

### CHANGELOG entry under `## Unreleased` → `### Added`

```markdown
- **Peer mailbox drain lifecycle events.** `WorkerPeerMessageDrainStarted`
  and `WorkerPeerMessageDrainTimedOut` add symmetric observability to the
  post-prompt ack drain. `DrainStarted` carries the candidate-set size
  and the cap/quiet-window limits in effect; `DrainTimedOut` mirrors the
  existing `WorkerPeerMessageDrainCappedOut` payload (with
  `quiet_window_ms` replacing nothing — both fields are present so
  dashboards can reuse panel queries). `DrainTimedOut` is emitted only
  when the quiet-window exit leaves remaining non-terminal messages;
  clean-exit drains (`remaining_messages == 0`) emit no exit event.
  Diagnostic-only — message loss continues to be tracked per-message
  via `WorkerPeerMessageIgnored`. (bd-cpf.7)
```

## What this fixes

1. **Drain-latency observability**: `DrainStarted` anchors latency dashboards. Today operators infer drain start from `PromptDispatched` or `DelegationCompleted`, which is imprecise under multi-drain or replay-flood scenarios.
2. **Drain-quietness anomaly visibility**: `DrainTimedOut` makes "worker stopped acking but did not consume all peer messages" a typed signal, not a per-`Ignored`-event aggregation puzzle.
3. **Drain-saturation denominator**: `candidates_at_start` is the independent variable for "how full was the mailbox when this drain began."
4. **Misconfig detection**: `cap_ms` on both exit events lets dashboards detect `quiet_window_ms >= cap_ms` deployments by comparing `actual_elapsed_ms ≈ cap_ms`.

## What this preserves

- All current behavior: drain still force-terminals on quiet-window expiry; `DrainCappedOut` semantics unchanged.
- Wire compat: `SpurEventBody` is `#[non_exhaustive]`; new variants are additive. Workspace consumers (`spur-bot`, `spur-tui`, `lineage::projection`) all use wildcard fallthrough — no breakage.
- Test discipline: bd-cpf.5b/5c precedent of round-trip + missing-field replay tests.
- `LateAckDropped` lifetime question: receiver-side observation is unreachable today (verified by all 3 reviewers via independent channel-ownership traces).

## Patch estimate

| File | LoC |
|---|---|
| `spur-acp/domain/events.rs` (2 new variants + 2 round-trip tests) | +60 |
| `spur-core/src/orchestrator.rs` (helper + 2 emit sites + 4 functional tests) | +60 |
| `CHANGELOG.md` entry | +10 |
| **Total** | **~+130 LoC** |

Risk: **low**. Strictly additive. The only behavioral change is when `DrainTimedOut` fires; existing tests for `DrainCappedOut` and `Ignored` events are unaffected.

## Followups (NOT bd-cpf.7 scope)

| Item | Tracking |
|---|---|
| `WorkerPeerMessageDrainFinished` (clean-exit denominator) | New ticket if dashboards request it (codex NICE-TO-HAVE) |
| `WorkerPeerMessageLateAckDropped` (late-ack observability) | Stage-2 — only if ack lifetime restructures (3-of-3 reviewers concur) |
| `WorkerPeerMessageAckReceived` (per-ack diagnostic) | Defer indefinitely — no concrete consumer (3-of-3 reviewers concur) |
| `FunnelHandle::emit_sampled` (volume control) | New infrastructure ticket if Stage-2 pushes funnel backpressure |
| `Limits` floor/ceiling validation (e.g., `quiet_window_ms < cap_ms`) | Defer; operators tune Limits directly today (kimi) |
