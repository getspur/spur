# bd-cpf.7 — additive peer-mailbox events: first-principles framing

## What bd-cpf.7 is

bd-cpf.7 is the "additive events" ticket from the original quad-review backlog. Per bd-cpf.4 synthesis (Followups table), the deferred candidates are:

1. `WorkerPeerMessageDrainStarted` — signal at drain begin (kimi mention).
2. `WorkerPeerMessageLateAckDropped` — signal for acks that arrive after the absolute-cap deadline.

The continuation note also mentioned `WorkerPeerMessageDrainTimedOut`. Reading the current `drain_peer_acks_with_timeout` at `crates/spur-core/src/orchestrator.rs:5240-5353`, there is a third real observability gap that the same ticket can naturally fold in:

3. `WorkerPeerMessageDrainTimedOut` — quiet-window exit (no acks for `drain_quiet_window_ms`) with remaining non-terminal messages still pending.

Today only the cap-hit exit emits a typed event (`WorkerPeerMessageDrainCappedOut`). The quiet-window exit silently force-terminals each remaining message with reason `"drain_timeout"` — operators can count `WorkerPeerMessageIgnored` events but lose the per-drain aggregate (how many messages were in the worker's mailbox at that moment, how long the drain took before going quiet).

## The current observability surface

```text
drain_peer_acks_with_timeout
  loop {
    timeout_at(min(quiet_deadline, cap_deadline), ack_rx.recv())
      Ok(Some(())) -> acks_received += 1            // [A] no event today
      Ok(None)     -> break (channel closed)
      Err(_)       -> break with cap_hit flag
  }

  if cap_hit {
    emit DrainCappedOut { acks_received, remaining_messages, cap_ms, actual_elapsed_ms }
                                                    // [B] typed event
  } else {
                                                    // [C] no event today (quiet-window exit)
  }

  for entry in candidates: record_terminal(Ignored, reason)  // per-message events
                                                    // reason = "drain_capped" | "drain_timeout"
```

Three places where additive observability is plausibly cheap:

- **Pre-loop**: a `DrainStarted` event with the candidate-set size and the cap/quiet-window values in effect. Lets dashboards correlate "drain started → drain ended" pairs and measure drain latency / saturation independently.
- **Position [C]**: a `DrainTimedOut` event symmetrical with `DrainCappedOut`, fired when the quiet-window timeout fires WITH remaining messages (i.e., a worker stopped acking but not all peer messages were consumed).
- **Post-loop**: a `LateAckDropped` event for the case where an ack arrives on a still-open `ack_rx` channel after the loop exited via cap_hit. Today the receiver is dropped on function return; if the sender side is still alive elsewhere, those acks become unobserved noise. (Need to verify whether the sender lifetime extends past drain exit — see Question 4.)

`AckReceived` per-ack (Position [A]) is plausibly desirable for fine-grained debugging but at a different cost profile (one event per ack × N drains × M workers = high event volume). Out of cheap-additive-scope IMO; flag for explicit reviewer judgment.

## Reachability today

- `DrainStarted`: every prompt-end drain. **High volume** (one per worker prompt) but low marginal-event cost (no per-message dimension).
- `DrainTimedOut`: fires on every drain that has remaining messages AND no acks for `drain_quiet_window_ms`. In Stage-1, this is the COMMON drain exit path — most drains have nothing to drain (DrainStarted with `candidates_at_start=0`) but those that do typically exit via quiet-window timeout, not cap. **Higher volume than DrainCappedOut**.
- `LateAckDropped`: only fires after `cap_hit` AND if the sender side is still alive AND if the worker actually emits a late ack. **Very rare** — diagnostic signal for misbehaving workers.
- `AckReceived`: every ack. **Highest volume** of any candidate.

## Why now (Stage-2 framing)

Stage-2 (persistent ledger, replay-flood reachability) introduces drain pressure that Stage-1 doesn't see. Today's drain typically exits with remaining_messages=0 and the quiet-window expiring naturally. Stage-2:
- Replay floods can land non-terminal messages just before drain start, creating drains with non-zero remaining_messages.
- Recurring/periodic reconciliation can race with drain.
- Multi-drain amplification (same `delegation_id`, multiple drains) becomes a real on-call scenario.

In all three, the operator's question is "did the drain time out / cap out / complete cleanly, and how much was outstanding when?" Today only cap-out is answerable; quiet-window-timeout-with-remaining-messages is answerable only by counting `Ignored` events with `reason="drain_timeout"` — strictly more work and harder to alert on.

## Cost analysis

| Event | New variant | New code | Test surface | Wire risk | Pager value today | Pager value Stage-2 |
|---|---|---|---|---|---|---|
| `DrainStarted` | yes | ~5 LoC at drain entry | 1 round-trip + emit assert | low (additive) | low | medium |
| `DrainTimedOut` | yes | ~5 LoC at position [C] | 1 round-trip + emit assert | low (additive) | medium | medium-high |
| `LateAckDropped` | yes | requires keeping `ack_rx` alive past drain exit OR a separate channel + drop hook | 1 round-trip + manufactured-late-ack test | low (additive) on wire; **medium on logic complexity** | low | low-medium |
| `AckReceived` | yes | ~3 LoC at position [A] | 1 round-trip | low (additive) | low (debug only) | low (still high volume) |

`LateAckDropped` is the odd one out — it's not a 5-LoC additive, because today the `ack_rx` is dropped on function exit. To "observe" a late ack we need to either:
- L1: Keep the receiver alive after `cap_hit`, drain remaining acks for some grace window, emit `LateAckDropped` per ack received during that grace.
- L2: Move the receiver/sender pair into a structure with a `Drop` impl that counts unread items.
- L3: Rebuild around a shared counter the sender increments and the cap-out event reads (no per-event signal, just a tally).

Pick the wrong one and `LateAckDropped` becomes its own behavioral change rather than additive observability. Reviewers should weigh whether `LateAckDropped` belongs in bd-cpf.7 at all or should be deferred until we have evidence the late-ack path actually fires in production.

## Wire-compat constraints

`SpurEventBody` is `#[non_exhaustive]`. Adding new variants is wire-compatible by construction. The `ReplayBody Known/Unknown` decoder gracefully handles unknown variants (verified across bd-cpf.3/4/5b/5c). Forward-replay tests (`worker_peer_event_tests` in `events.rs`) must include a deserialize-with-missing test for any new field.

`spur-tui` and `spur-mcp` consumers of `SpurEventBody` are NOT exhaustive (they match only the variants they care about). New variants are silently ignored downstream until a consumer opts in.

## Design questions

1. **Scope**: which of the four candidates (`DrainStarted`, `DrainTimedOut`, `LateAckDropped`, `AckReceived`) should bd-cpf.7 ship? My provisional pick is `DrainStarted` + `DrainTimedOut` (cheap-additive, symmetric with existing `DrainCappedOut`, real Stage-2 value); defer `LateAckDropped` (needs lifetime work) and `AckReceived` (high volume, no clear consumer).

2. **`DrainTimedOut` field shape**: should it carry the same payload as `DrainCappedOut` (`acks_received`, `remaining_messages`, `cap_ms`, `actual_elapsed_ms`) plus a `quiet_window_ms`? Or a slimmer payload (`remaining_messages`, `actual_elapsed_ms`)? Symmetry argues for full payload.

3. **`DrainStarted` field shape**: `brain_session_id`, `target_delegation_id`, `candidates_at_start: u32`, `cap_ms: u64`, `quiet_window_ms: u64`. Anything else?

4. **`LateAckDropped` lifetime question**: is the `ack_rx` sender side still alive after `drain_peer_acks_with_timeout` returns? Where does the sender live (`PeerMailboxBundle`?), and is the receiver re-installed for the next drain or recreated? If the answer is "sender goes away with the receiver at function return", then there ARE no late acks — the channel is closed, and `LateAckDropped` is observability for nothing.

5. **`DrainTimedOut` vs `DrainCappedOut` on the no-remaining path**: today, when the quiet-window exits and `remaining_messages == 0`, the drain completed cleanly. Should we emit `DrainTimedOut` only when `remaining_messages > 0`? Or unconditionally on quiet-window exit (so dashboards can compute "drains that ended cleanly = `DrainTimedOut` events with remaining=0")? Symmetry with `DrainCappedOut` (which fires regardless of remaining) argues for unconditional emission.

6. **Volume control**: `DrainStarted` fires once per worker prompt. At sustained load this may be high-volume. Is there a sampling/aggregation knob already in `event_funnel`, or do we accept the volume?

7. **Tests**: minimum coverage = wire-roundtrip (serialize + replay-deserialize-missing-fields) per new variant + 1 functional test per new event (drain reaches `DrainStarted`, drain exits via quiet-window with remaining → `DrainTimedOut`).

## Asks for reviewers

- Pick scope: my proposed minimal scope is `DrainStarted` + `DrainTimedOut`. Defend or override.
- For each in-scope event, validate the field shape.
- Settle Q4 (`LateAckDropped` lifetime): if you have evidence the late-ack path is reachable, please describe it; otherwise endorse defer.
- Patch size estimate for your preferred scope.
