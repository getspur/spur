# bd-cpf.7 design review (codex)

## Recommendation

Ship `WorkerPeerMessageDrainStarted` and `WorkerPeerMessageDrainTimedOut` in
bd-cpf.7. Defer `WorkerPeerMessageLateAckDropped` and
`WorkerPeerMessageAckReceived`.

This is the right Rust-idiom boundary: two additive, bounded-cardinality events
on the existing drain path, no new ownership model, no sender-side state
machine, and no per-ack event stream. The implementation should keep all new
event variants public because `SpurEventBody` is the public wire surface; if an
event is not meant for downstream consumers, use `tracing`, not a private enum
variant.

Recommended event shapes:

```rust
WorkerPeerMessageDrainStarted {
    brain_session_id: String,
    target_delegation_id: DelegationId,
    candidates_at_start: u32,
    cap_ms: u64,
    quiet_window_ms: u64,
}

WorkerPeerMessageDrainTimedOut {
    brain_session_id: String,
    target_delegation_id: DelegationId,
    acks_received: u32,
    remaining_messages: u32,
    quiet_window_ms: u64,
    actual_elapsed_ms: u64,
}
```

Do not add `AckReceived` in this ticket. `acks_received` on the aggregate
events is the right cardinality for operations; per-ack diagnostics can come
later behind an explicit debug/trace facility.

## Classifications

BLOCKER:
- Do not implement `WorkerPeerMessageLateAckDropped` as a five-line variant.
  The current `ack_rx` lifetime cannot observe the important late cases without
  additional state or an extended receiver lifetime.
- Keep all new `SpurEventBody` consumers non-exhaustive. `SpurEventBody` is
  already `#[non_exhaustive]`, but the defining crate can still write
  exhaustive matches in tests; new tests should use `if let`, `matches!`, or a
  wildcard arm.
- Add serde round-trip coverage for each new variant. If any new field is added
  to an existing variant, it must carry `#[serde(default)]`.

SHOULD-DO:
- Emit `DrainStarted` after taking a deduplicated candidate snapshot at drain
  entry. Reuse a helper for "candidate entries for target" so start and end
  counts cannot drift by copy-paste.
- Emit `DrainTimedOut` only for quiet-window exits with
  `remaining_messages > 0`. Quiet-window expiry is normal control flow; the
  typed event should mean "the drain forced terminal messages after going
  quiet."
- Put `quiet_window_ms` on `DrainTimedOut`, not on the existing
  `DrainCappedOut`, to avoid changing an existing variant in bd-cpf.7.
- Functional tests should use the existing `#[tokio::test(start_paused = true)]`
  style with `tokio::time::advance`, matching the current drain tests.

NICE-TO-HAVE:
- Add a doc comment parallel to `WorkerPeerMessageDrainCappedOut`: diagnostic
  only, not message-loss accounting. Loss remains the per-message
  `WorkerPeerMessageIgnored` event.
- Consider a future `WorkerPeerMessageDrainFinished` if dashboards need a clean
  completion denominator. Do not overload `DrainTimedOut { remaining_messages:
  0 }` for that.

Patch-size estimate for the preferred scope: about 100-150 LoC across
`crates/spur-acp/src/domain/events.rs` and
`crates/spur-core/src/orchestrator.rs`, plus tests. If the candidate snapshot is
factored cleanly, expect closer to 150 LoC; if duplicated inline, less code but
worse maintenance.

## Q1. Scope

Scope should be `WorkerPeerMessageDrainStarted` plus
`WorkerPeerMessageDrainTimedOut`.

`DrainStarted` is a bounded, one-per-drain event. It gives operators the input
size and configured timing knobs before any acks arrive. `DrainTimedOut` closes
the current asymmetry where cap-hit exits get `WorkerPeerMessageDrainCappedOut`
but quiet-window exits only produce per-message `Ignored` events with
`reason = "drain_timeout"`.

`LateAckDropped` is not additive at the same complexity level. The receiver is
owned by `drain_peer_acks_with_timeout` and consumed by value, so observing late
acks requires a lifetime/state design. `AckReceived` is per-ack and high-volume;
the aggregate `acks_received` counter already covers the operational question
for this ticket.

## Q2. `DrainTimedOut` Field Shape

Use the same identity and aggregate fields as `DrainCappedOut`, but with the
quiet-window knob instead of the cap knob:

- `brain_session_id`
- `target_delegation_id`
- `acks_received`
- `remaining_messages`
- `quiet_window_ms`
- `actual_elapsed_ms`

I would not include `cap_ms` on `DrainTimedOut`. The cap did not fire, and the
field invites consumers to compare unrelated thresholds. If a shared schema is
more important than semantic precision, include both `cap_ms` and
`quiet_window_ms`, but that is slightly noisier and does not buy much in Stage 1.

## Q3. `DrainStarted` Field Shape

The proposed shape is good:

- `brain_session_id`
- `target_delegation_id`
- `candidates_at_start`
- `cap_ms`
- `quiet_window_ms`

The implementation detail that matters is `candidates_at_start`: compute it
from the same deduplicated candidate set used at drain end. Today
`drain_peer_acks_with_timeout` builds candidates from
`pending_for_target(delegation_id)` plus `non_terminal_entries()` filtered by
target, then deduplicates by `message_id`. Pull that into a small helper inside
`orchestrator.rs` rather than writing the same logic twice.

## Q4. `LateAckDropped` Lifetime Trace

Current ownership path:

1. Each reviewed worker attempt creates a fresh unbounded channel:
   `let (ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();` in
   `run_worker_with_retries`.
2. A clone of `ack_tx` is passed into `WorkerAttemptCtx`, then cloned into the
   worker ext-notification task.
3. `_spur/peer_message_consumed` and `_spur/peer_message_ignored` call
   `interpret_peer_message_terminal`, which records the terminal state and then
   does `let _ = ack_tx.send(())`.
4. After the worker attempt returns, the orchestrator drops its local
   `ack_tx`, then passes `ack_rx` by value into
   `drain_peer_acks_with_timeout`.
5. `drain_peer_acks_with_timeout` stops polling `ack_rx` when the quiet window
   or cap fires. On function return, the receiver is dropped.

So, can late acks occur? Semantically yes: the worker-side ext notification
task can still receive a terminal notification after the drain loop has decided
to stop waiting. But the current channel design makes most of those acks
unobservable:

- If the ack arrives after `drain_peer_acks_with_timeout` returns,
  `ack_tx.send(())` fails because the receiver is gone. The current code
  intentionally ignores that error.
- If the ack arrives after the loop exits but before the function returns, the
  send can still succeed into an `ack_rx` that is alive but no longer polled.
  That ack is silently dropped when the receiver is dropped.

The existing unit test `drain_late_ack_after_timeout_is_safely_swallowed`
already captures the after-return case by asserting that a stale sender returns
`Err` once the drain receiver exits. That is strong evidence that
`LateAckDropped` is not reachable as a simple receiver-side event after the
current function returns.

Cheapest path if `LateAckDropped` becomes mandatory later:

- Prefer a fourth option over the framing's L1/L2/L3: introduce a tiny
  per-attempt `AckDrainState` shared by the sender-side terminal interpreter
  and the drain. The drain records its terminal state (`Open`, `QuietTimedOut`,
  `CapHit`, `Closed`) plus the target delegation context. The sender helper
  emits `LateAckDropped` when `send` fails or when state says the drain is no
  longer polling.
- This is still non-trivial because it crosses `orchestrator.rs` and
  `spur_ext_interp.rs`, needs a race-proof state transition, and must decide
  whether quiet-window late acks count or only cap-hit late acks count.
- L1, keeping `ack_rx` alive for a grace window after cap, is cheaper to code
  but incomplete: it only observes acks during that grace period and still drops
  after-return sends silently. I would not ship it unless the event name is
  explicitly scoped to "late ack observed during cap grace."

For bd-cpf.7, defer again. The review framing is correct that this is lifetime
work, not additive event plumbing.

## Q5. `DrainTimedOut` On The No-Remaining Path

Emit `DrainTimedOut` only when the quiet-window exit leaves
`remaining_messages > 0`.

Quiet-window expiry is how a healthy drain finishes. Calling the clean path
`TimedOut` makes dashboards and logs harder to reason about and doubles event
volume when paired with `DrainStarted`. `DrainCappedOut` is different: hitting
the absolute cap is itself anomalous even if a racing ack consumed the last
message before the final snapshot.

If consumers need a success denominator, add a later `DrainFinished` event with
an explicit `exit_reason` rather than encoding success as a timeout event with
zero remaining messages.

## Q6. Volume Control

There is no sampling or aggregation knob in `event_funnel`. It is an unbounded
mpsc into a stamping task and broadcast channel.

Given that, keep bd-cpf.7 bounded:

- `DrainStarted`: one per reviewed worker attempt with peer mailbox enabled.
- `DrainTimedOut`: only quiet-window exits with remaining messages.
- No `AckReceived`: per-ack event volume is the wrong default.

If `DrainStarted` volume becomes noisy under Stage 2 replay load, the mitigation
should be consumer-side filtering or a later explicit event-policy layer. Do
not smuggle sampling into the drain code.

## Q7. Tests

Minimum tests:

1. `worker_peer_event_tests` round-trip for `WorkerPeerMessageDrainStarted`.
2. `worker_peer_event_tests` round-trip for `WorkerPeerMessageDrainTimedOut`.
3. A functional drain test using `#[tokio::test(start_paused = true)]` that
   asserts `DrainStarted` emits `candidates_at_start`.
4. A functional drain test using `tokio::time::advance` that exits through the
   quiet window with a remaining delivered or delivered-inflight message and
   asserts exactly one `DrainTimedOut`.
5. A negative functional assertion that quiet-window exit with
   `remaining_messages == 0` does not emit `DrainTimedOut`, if the recommendation
   above is accepted.

For serde default discipline:

- New variants do not need `#[serde(default)]` on every required field merely
  because the variant is new.
- Any field added to an existing variant must use `#[serde(default)]` and have
  a missing-field deserialize test.
- Optional or future-extension fields on new variants should also use
  `#[serde(default, skip_serializing_if = "...")]`.

## Match-Arm Completeness

The workspace-internal consumers I found already use wildcard arms for
`SpurEventBody` matches:

- `spur-core::lineage::projection` explicitly ignores current peer-mailbox
  lifecycle events and then has `_ => {}`.
- `spur-bot::runtime` handles only bot-visible events and falls through to
  `_ => Ok((None, vec![]))`.
- TUI event handling similarly uses targeted arms plus wildcard fallthrough.

Because `SpurEventBody` is `#[non_exhaustive]`, external crates are forced into
non-exhaustive matching. The main breakage risk is inside `spur-acp` tests,
where the defining crate may still write exhaustive matches. The existing test
style already mostly avoids that; keep new tests in that style.
