# bd-cpf.4 — drain absolute-cap: first-principles framing

## The bug

`drain_peer_acks_with_timeout` (`crates/spur-core/src/orchestrator.rs:5248-5303`) loops:

```rust
loop {
    match tokio::time::timeout(quiet_window, ack_rx.recv()).await {
        Ok(Some(())) => continue,                  // ack reset the window
        Ok(None) | Err(_) => break,                // sender drop OR timeout
    }
}
```

Every received ack RESETS the quiet window. A chatty worker that sends acks faster than `quiet_window` (default 2s, `Limits::drain_quiet_window_ms`) keeps the drain alive indefinitely. Per kimi's bd-cpf.1 ops review:

> drain reset-timer has no absolute cap — chatty worker keeps drain alive forever

## What's at risk

- **Delegation cleanup blocks**: `run_one_worker_attempt` returns; the next code path (delegation finalization) is gated on drain finishing. A wedged drain wedges the orchestrator's per-attempt FSM.
- **Resource hold**: `ack_rx` (UnboundedReceiver) keeps the channel alive; the bundle's references to ledger/router stay rooted.
- **Pager invisibility**: there's no event for "drain hit absolute cap" today. Operators see a delegation that "never finishes" without a typed signal.
- **Test impossibility**: any deterministic test that wants to bound drain duration is currently impossible (no cap exists).

## The reachability question

Is this exploit-only, or is it reachable in normal operation?

- **Normal fast worker**: emits N acks for N peer messages within ~milliseconds. `quiet_window=2s`. Drain exits ~2s after the last ack.
- **Slow worker**: same, but acks spaced >2s apart. Drain exits 2s after each isolated ack.
- **Chatty / pathological worker**: emits acks at >0.5Hz forever. Drain never exits.
- **Buggy/malicious worker code**: spawns an ack-emitter task that doesn't stop after legitimate work. Drain pinned forever.

A chatty worker is _not_ exploit-only — a buggy retry loop in worker-side ack emission would do this. And once the persistent ledger lands (Stage-2), restart-recovery flows might replay ack streams, increasing exposure.

## Design questions

1. **What value should the absolute cap be?**
   - Constant (e.g. `5 × quiet_window` = 10s)?
   - Separate Limits field (`drain_max_total_ms`)?
   - Function of message count (e.g. `quiet_window × min(pending, 16)`)?
2. **What happens on cap hit?**
   - Force-terminal everything in-flight (same as quiet-window timeout today) ✓ baseline.
   - Different terminal reason? (`drain_capped` vs `drain_timeout`)?
   - Emit a typed event (`WorkerPeerMessageDrainCappedOut`)?
3. **How to thread the cap?**
   - New field in `Limits` and read from `bundle.router.limits()` inside the drain (consistent with `drain_quiet_window_ms`).
   - Hardcoded constant in the drain function (simplest).
   - Function parameter at call sites (4 call sites in orchestrator.rs).
4. **What's the right loop shape?**
   - **A**: Track `start = Instant::now()`; in each iteration compute remaining budget = `abs_cap - start.elapsed()`; pass `min(quiet_window, remaining)` to `timeout()`.
   - **B**: `tokio::select!` over `ack_rx.recv()`, `tokio::time::sleep(quiet_window)`, `tokio::time::sleep(abs_cap_remaining)`.
   - **C**: Two-task split — one drains acks, one is an unconditional `tokio::time::sleep(abs_cap)` that aborts the drain.
   - **D**: Bounded-iteration loop — cap on _number_ of acks consumed (e.g. `max_acks = depth × 2`), no time cap.

## Alternatives summary

### Alt A — Simple deadline (`min(quiet_window, remaining_budget)`)

Track start; compute remaining each iteration. Pass `min(quiet_window, remaining)` to `tokio::time::timeout`. On timeout: break. On `Ok(Some(()))`: continue (window resets, but absolute cap shrinks). On `Ok(None)`: break.

**Pro**: Single-loop, single `await`, deterministic with `tokio::time::pause`. Smallest cognitive load.
**Con**: Slight skew possible if `recv()` returns slowly — but bounded by `abs_cap + quiet_window` worst case.

### Alt B — `tokio::select!` with deadline arm

```rust
let deadline = tokio::time::sleep(abs_cap);
tokio::pin!(deadline);
loop {
    tokio::select! {
        _ = &mut deadline => break,
        result = tokio::time::timeout(quiet_window, ack_rx.recv()) => {
            match result {
                Ok(Some(())) => continue,
                _ => break,
            }
        }
    }
}
```

**Pro**: Visible deadline as a separate arm; deadline pinned ahead of loop.
**Con**: Three branches; learners pay select! cognitive tax. Slightly more code.

### Alt C — Bounded ack count

No time cap; cap on _number_ of acks consumed (`max_acks = max_pending_mailbox_depth × 2 = 16` default).

**Pro**: Bounds work, not just time. Argues "we don't care about a drain that's making progress; we care about one that's going forever."
**Con**: A worker that floods 17 acks then goes quiet would still have legitimate trailing acks dropped after the cap. Conflates progress with malice.

### Alt D — Per-message timeout

Replace whole-drain timer with per-message in-flight timer. Each in-flight message has its own deadline; drain finishes when all in-flight messages are terminal-or-deadline-hit.

**Pro**: Precise — no message starves another.
**Con**: Massive rewrite. Wrong scope for Stage-1.

## First-principles questions

1. **What's the actual SLO?** What's the maximum acceptable drain duration before delegation cleanup unblocks? If it's "10 seconds," that picks the cap. If it's "depends on backlog," it suggests a function of pending depth.
2. **What's the failure mode if cap is too tight?** Force-terminal a message that was about to legitimately ack. The message becomes `Ignored("drain_timeout")` instead of `Consumed`. Audit-noisy but not corrupting (`Ignored` is a valid terminal state).
3. **What's the failure mode if cap is too loose?** Drain wedges per the bug. Worse failure mode than tight cap.
4. **Default vs. override?** Should the cap be configurable (`Limits::drain_max_total_ms`) or fixed? Configurable adds tuning surface; fixed simplifies reasoning.
5. **Observability — typed event?** Adding `WorkerPeerMessageDrainCappedOut` lets operators distinguish "trickle-stopped" (per-ack quiet-window expiry) from "absolute-cap hit" (drain wedged). Worth a single new variant.

## My provisional recommendation

**Alt A** with these specifics:
- Add `drain_max_total_ms: u64` to `Limits` (default `5 * drain_quiet_window_ms` = 10_000).
- Track `start = tokio::time::Instant::now()` before loop.
- Each iteration: `remaining = max_total.saturating_sub(start.elapsed())`. If 0, break with `cap_hit = true`. Else pass `min(quiet_window, remaining)` to `timeout`.
- After loop, if `cap_hit`, emit a single `WorkerPeerMessageDrainCappedOut { brain_session_id, target_delegation_id, drained_acks, pending_remaining }` (cardinality-bounded fields).
- Force-terminal sweep stays the same (already runs after the loop).

Patch size: ~30 LoC + ~30 LoC test + 1 event variant + projection no-op arm.

## Asks for reviewers

1. Pick A/B/C/D or your own E. Why?
2. Cap value and shape: constant? Limits field? Function of depth?
3. Should cap-hit emit a typed event, or is the existing `WorkerPeerMessageIgnored("drain_timeout")` sweep sufficient?
4. Test strategy — `tokio::time::pause` + simulated ack flood, or property-based?
5. Patch size and risk for your preferred alternative.
