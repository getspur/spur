# bd-cpf.4 design synthesis — Alt E' (codex's deadline + kimi's observability)

After 12-round L9 sequential-thinking MCTS over the three design reviews, the chosen design is **Alt E'**: codex's `tokio::time::timeout_at` deadline shape combined with kimi's classification + telemetry amendments.

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Loop shape | Alt B `select! biased` | Alt A | Alt E `timeout_at` | **Alt E (codex)** | gemini |
| Default cap value | dynamic | 30s | 10s | **10s** | kimi |
| Dynamic-by-pending-depth | yes | no | no | **no (fixed)** | gemini |
| Distinct sweep reason | yes | **BLOCKER** | yes | **`"drain_capped"`** | converged |
| `REASON_ALLOWLIST` update | (implicit) | required | (implicit) | **yes** | kimi-specific |
| Typed `DrainCappedOut` event | yes | yes | yes-or-reason | **yes** | converged |
| Event includes `cap_ms` | (no) | **BLOCKER** | open Q | **yes** | kimi-specific |
| Event includes `actual_elapsed_ms` | (no) | **BLOCKER** | open Q | **yes** | kimi-specific |
| Lineage projection no-op arm | (implicit) | yes | (implicit) | **yes** | converged |
| Limits floor/ceiling validation | (no) | floor=10s, ceiling=120s | validate `>0` | **validate `>0`** | kimi |
| Multi-drain amplification | (no) | mention | (no) | **defer (separate ticket)** | kimi |
| Restart-replay floods | (no) | mention | (no) | **defer (Stage-2)** | kimi |
| Drain-start event | (no) | mention | (no) | **defer (bd-cpf.7)** | kimi |

## Override rationale

1. **gemini's `select! biased` → codex's `timeout_at`**: `tokio::time::timeout_at(deadline, recv())` gives same boundedness with one fewer construct, no biased-poll subtlety, and explicit `cap_hit` classification via `waiting_for_cap = next_deadline == cap_deadline`. Test ergonomics also better — no `&mut sleep` pinning across iterations.

2. **gemini's dynamic-by-depth cap → fixed cap**: codex's argument wins decisively — channel depth is a weak proxy for legitimate work; the ledger is truth. The correct response to "legitimate slow worker hit cap" is to raise the configured Limits value, not to scale by depth (which couples correctness to a non-protocol field).

3. **kimi's 30s default → 10s default**: The drain runs *after* the worker task completes. Its job is to catch trailing ack notifications from the worker's tool-call epilogue (~ms in normal operation). Kimi's slow-legitimate-worker scenario (8 messages × 1.5s/ack) implies the worker is still emitting acks during drain — which means the worker hasn't actually returned, which means we shouldn't be in drain phase. If a worker's background tasks keep emitting acks after the main task returns, that *is* the buggy/chatty case the cap should catch. 10s = 5× quiet_window is enough for residual-ack drain plus margin; 30s would extend pathological wedge time 3×. The typed event lets ops observe and tune via Limits without code change.

4. **kimi's floor=10s/ceiling=120s → validate `>0` only**: Hard-clamping operator config is paternalistic; test configs may use 100ms, extreme deployments may legitimately want >120s. Validate `>0` at construction; warn if `cap < quiet_window` ("the cap wins" mode).

5. **Threat-model expansions deferred**: Multi-drain amplification, restart-replay floods, drain-start events, and `LateAckDropped` events are real concerns but bigger scope. bd-cpf.4's mandate is the single-drain wedge per kimi's original bd-cpf.1 finding. Deferred items tracked in this doc's "Followups" section.

## The Alt E' design

### Loop shape (in `orchestrator.rs::drain_peer_acks_with_timeout`)

```rust
let cap_deadline = tokio::time::Instant::now() + max_total;
let mut cap_hit = false;
let mut acks_received: u32 = 0;
let drain_start = std::time::Instant::now();

loop {
    let now = tokio::time::Instant::now();
    if now >= cap_deadline {
        cap_hit = true;
        break;
    }
    let quiet_deadline = now + quiet_window;
    let next_deadline = quiet_deadline.min(cap_deadline);
    let waiting_for_cap = next_deadline == cap_deadline;

    match tokio::time::timeout_at(next_deadline, ack_rx.recv()).await {
        Ok(Some(())) => { acks_received = acks_received.saturating_add(1); }
        Ok(None) => break,
        Err(_) => { cap_hit = waiting_for_cap; break; }
    }
}

let actual_elapsed_ms = drain_start.elapsed().as_millis() as u64;

// Emit typed cap event (once) BEFORE sweep:
if cap_hit {
    funnel.emit(SpurEventBody::WorkerPeerMessageDrainCappedOut {
        brain_session_id: brain_session_id.to_string(),
        target_delegation_id: delegation_id.clone(),
        acks_received,
        remaining_messages: /* computed below from candidates len */,
        cap_ms: max_total.as_millis() as u64,
        actual_elapsed_ms,
    });
}

// Sweep (existing logic, with reason switched on cap_hit):
let reason = if cap_hit { "drain_capped" } else { "drain_timeout" };
// ... force-terminal pass over candidates with TerminalOutcome::Ignored { reason: reason.into() }
```

### `Limits` field

```rust
// crates/spur-core/src/peer_mailbox/limits.rs
pub struct Limits {
    // ... existing fields
    pub drain_max_total_ms: u64,       // NEW
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // ... existing defaults
            drain_max_total_ms: 10_000,
        }
    }
}
```

Validation: a constructor or builder asserts `drain_max_total_ms > 0`; logs a `tracing::warn!` if `drain_max_total_ms < drain_quiet_window_ms`.

### `REASON_ALLOWLIST` update

```rust
// crates/spur-core/src/spur_ext_interp.rs
const REASON_ALLOWLIST: &[&str] = &[
    "worker_ignored",
    "drain_timeout",
    "drain_capped",       // NEW
    "out_of_scope",
    "duplicate",
    "stale_plan_version",
];
```

### Typed event

```rust
// crates/spur-acp/src/domain/events.rs (in SpurEventBody enum)
WorkerPeerMessageDrainCappedOut {
    brain_session_id: String,
    target_delegation_id: crate::domain::delegation::DelegationId,
    acks_received: u32,
    remaining_messages: u32,
    cap_ms: u64,
    actual_elapsed_ms: u64,
},
```

Doc comment: "Diagnostic-only. Do NOT count in message-loss metrics — message loss is counted via `WorkerPeerMessageIgnored` per-message events. Use this for drain-health / worker-behavior dashboards."

`lineage/projection.rs::ExecutorLineage::apply` adds a no-op match arm alongside the existing peer-event no-op group.

### Function signature change

Drain function gains `max_total: Duration`, `brain_session_id: &str`, and `funnel: FunnelHandle` (or `&FunnelHandle`) parameters. All four call sites (`orchestrator.rs:1148, 2538, 2804, 3908`) update accordingly — `brain_session_id` and `funnel` are already in scope at each.

### Tests

1. **`drain_hits_cap_under_ack_flood`** (`#[tokio::test(start_paused = true)]`):
   - Setup: ledger with 1 in-flight `Delivered` message; quiet_window=1s, max_total=5s.
   - Loop: send ack, advance 900ms, yield, repeat 5 times (= 4500ms simulated, 5 acks below quiet window).
   - Advance another 499ms, yield → drain still alive.
   - Advance 1ms → drain exits at cap.
   - Assert: drain elapsed ∈ [5000ms, 5050ms]; ledger entry now `Ignored` with reason `"drain_capped"`; exactly one `WorkerPeerMessageDrainCappedOut` emitted with `cap_ms=5000`; `acks_received=5`.

2. **`drain_quiet_exit_under_normal_flow`** (`#[tokio::test(start_paused = true)]`):
   - Setup: ledger with 1 in-flight message; quiet_window=1s, max_total=10s.
   - Send ack, advance 100ms, send ack, advance 100ms, send ack.
   - Advance 1100ms (quiet window expires).
   - Assert: drain exited cleanly; sweep reason `"drain_timeout"`; no `DrainCappedOut` event.

3. **(regression update)** Existing `drain_resets_quiet_window_on_each_ack` test gets a generous `max_total=60s` so it still demonstrates the reset semantic without accidentally hitting the new cap.

4. **Event round-trip** (in `events.rs::worker_peer_event_tests`): standard serde round-trip for `WorkerPeerMessageDrainCappedOut` with all fields populated.

## Patch estimate

| File | LoC |
|---|---|
| `peer_mailbox/limits.rs` | +3 (field, default, optional validation) |
| `orchestrator.rs::drain_peer_acks_with_timeout` | +30 (loop rewrite + emit) |
| `orchestrator.rs` call sites × 4 | +12 |
| `spur_ext_interp.rs` REASON_ALLOWLIST | +1 |
| `spur-acp/domain/events.rs` (variant + round-trip test) | +25 |
| `lineage/projection.rs` no-op arm | +1 |
| New drain tests (×2) | +90 |
| Existing test update | +5 |
| **Total** | **~165 LoC** |

Risk: **low**. The change only bounds previously-unbounded behavior. The cap-hit path force-terminals the same set of messages the existing quiet-window timeout already does — only the reason and an extra typed event differ.

## Followups (NOT bd-cpf.4 scope)

| Item | Tracking |
|---|---|
| Multi-drain amplification (rate-limit retry of same delegation_id) | New ticket if observed in production |
| Restart-replay flood pre-drain dedup | Stage-2 persistent-ledger work |
| `WorkerPeerMessageDrainStarted` event | bd-cpf.7 (additive events) |
| `LateAckDropped` event for post-cap acks | bd-cpf.7 |
| `Limits` floor/ceiling validation | Defer; operators tune Limits values directly |
| proptest dev-dep for boundary fuzzing | Stage-2 stability work |

## What this preserves

- Matrix invariants: zero changes to `is_valid_transition`. `Ignored` is already a valid terminal state from `Delivered`/`DeliveredInflight`.
- Existing drain contract: idempotent, force-terminals on quiet-window expiry, late acks become `Ignored`.
- Wire compat: new event variant is purely additive (`SpurEventBody` is `#[non_exhaustive]`-ish; ReplayBody Known/Unknown handles unknown variants gracefully). New `Limits::drain_max_total_ms` field has a default; existing instantiations pick up 10_000 transparently.
- Stage-2 trajectory: typed event signal becomes more valuable when persistent ledger introduces replay-flood reachability; cap is the safety valve regardless.
