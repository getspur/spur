# bd-cpf.3 — drain/reconciler unification: first-principles framing

## The two functions

**`drain_peer_acks_with_timeout`** (`crates/spur-core/src/orchestrator.rs:5197`)
- Triggered: after `run_one_worker_attempt` returns, before delegation cleanup
- Scope: ONE delegation (the one that just finished)
- Behavior: `tokio::time::timeout(quiet_window, ack_rx.recv())` loop; each ack RESETS the window; on timeout/sender-drop, force `Delivered|DeliveredInflight → Ignored("drain_timeout")` for that delegation's messages
- Effect on ledger: bounded — only this delegation's in-flight messages

**`run_startup_reconcile`** (`crates/spur-core/src/peer_mailbox/reconciler.rs:16`)
- Triggered: orchestrator startup (after potential prior crash)
- Scope: ALL non-terminal entries in the ledger
- Behavior: walks the ledger, transitions in-flight messages back to `Queued` (if not injected) or forward to `Delivered` (if injected), increments counters
- `drain_quiet_window` parameter accepted but **silently discarded** (TODO at reconciler.rs:22)
- Effect on ledger: global — every delegation's in-flight messages

## Reviewer convergence

Three of four quad-reviewers (claude-code, kimi, gemini) flagged these as "convergent design that should be unified". My current issue brief proposed:

```
fn drain_with_quiet_window(scope: DrainScope, behavior: DrainBehavior, quiet_window: Duration, ...)
where DrainScope ∈ {SingleDelegation(DelegationId), AllNonTerminal}
      DrainBehavior ∈ {ForceImmediate, WaitQuietWindow}
```

A god-function with two enum dimensions = 4 combinations. Two are real (orchestrator drain = SingleDelegation+WaitQuietWindow; reconciler today = AllNonTerminal+ForceImmediate; reconciler-after-fix = AllNonTerminal+WaitQuietWindow). One is hypothetical (SingleDelegation+ForceImmediate — useful?).

## Alternatives

### Alt A — Full unification (issue brief's original proposal)
Merge into one function with the two enum dimensions. Keep both call sites; pass different params.

**Pro:** Eliminates duplication. One function = one behavioral contract.
**Con:** Conflates two distinct semantics. Future readers must mentally re-bind the function's behavior every time.

### Alt B — Shared helper, distinct callers
Extract `force_terminal_for_inflight(ledger, scope_filter, reason) -> ForcedReport` and `await_acks_with_quiet_window(ack_rx, quiet_window, abs_cap) -> AckOutcome`. Both `drain_peer_acks_with_timeout` and `run_startup_reconcile` call these helpers but stay distinct top-level functions.

**Pro:** Each caller stays semantically clear ("drain for delegation X" vs. "reconcile after restart"). Helpers unit-testable.
**Con:** Two helpers = more API surface. Doesn't reduce caller LoC as much.

### Alt C — Don't unify; ONLY fix the reconciler bug
The 3-way reviewer convergence was on the *symptom* (duplication). The root *bug* is that `run_startup_reconcile` ignores `drain_quiet_window` AND that worker acks during reconcile-revert race silently fail (kimi's pager finding from bd-cpf.1 ops review). Maybe just fix the bug, defer unification.

Specifically: make `run_startup_reconcile` SKIP messages that have a live worker still acking (detected via a heartbeat or a recent-event timestamp), OR run a quiet window before forcing terminal. Leave drain code alone.

**Pro:** Smallest patch. Doesn't touch orchestrator.rs (already-stable code). Targets the actual production risk.
**Con:** Leaves duplication. The next reviewer pass will flag it again.

### Alt D — Extract shared helper + ALSO add quiet-window to reconciler
Combine B + the bug-fix scope. Two helpers. Both callers updated. Reconciler gains quiet-window enforcement.

**Pro:** Closes the bug AND reduces duplication.
**Con:** Largest patch.

## First-principles questions

1. **Is duplication actually bad here?** The drain is in the orchestrator's per-attempt cleanup path. The reconciler runs once at startup. They have no overlapping invocation context. Two functions ≠ duplication if they encode different control-flow situations.

2. **What's the actual production bug?** Per kimi's bd-cpf.1 review: orchestrator restart → reconciler reverts in-flight messages to Queued → late worker ack tries `Queued → Consumed` → invalid per matrix → ack swallowed → message stranded. The fix REQUIRED is making the reconciler tolerant of late acks. Unification is incidental.

3. **Should the reconciler wait?** Waiting on startup adds boot latency. If we wait `drain_quiet_window` for every in-flight message, recovery time scales with `O(messages × quiet_window)` worst case. Unbounded boot delays are an anti-pattern. Better: spawn workers anyway, let normal ack flow handle them, and only force-terminal messages that don't ack within a separate background pass.

4. **Is reverting to Queued even right?** Per matrix: in-flight messages have already been INJECTED into a worker's prompt. Reverting to Queued means "we'll re-inject in the next prompt" — but the worker may still be processing the original injection. We're double-spending the worker's context.

## My provisional recommendation: Alt C (focused bug fix)

Don't unify. Instead:
- Make `run_startup_reconcile` more conservative: instead of forcing `DeliveredInflight → Queued`, mark messages as `DeliveredInflight` and rely on the orchestrator's normal post-prompt code (now post-bd-cpf.1) to terminalize them when workers ack.
- Add an optional `WorkerPeerMessageReconciledStranded` event for messages that the reconciler can't reason about (e.g., the worker session is gone, no recovery path).
- Defer unification to Stage-2 when the persistent ledger exposes per-message metadata (created_at, last_event_at) that makes "is this message still alive?" a typed query rather than a guess.

## Asks for reviewers

1. Which alternative (A, B, C, D, or your own E) is the right scope for *Stage-1* given:
   - The 3-way reviewer convergence on "unify" was about code shape, not the underlying bug
   - Stage-2 will replace the in-memory ledger anyway → big refactors here are write-offs
   - The actual production risk is the reconciler-vs-late-ack race
2. Concrete failure scenarios that each alternative leaves uncovered.
3. Estimate of patch size and risk for your preferred alternative.
