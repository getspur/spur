# bd-cpf.5c — reconciler `Ok(_)` Changed/Unchanged collapse: first-principles framing

## The bug (as flagged in bd-cpf.5/5b reviews)

`crates/spur-core/src/peer_mailbox/reconciler.rs:76` collapses `Changed` and `Unchanged(_)` into one arm:

```rust
TransitionAuditOutcome::Changed | TransitionAuditOutcome::Unchanged(_) => {
    counts.inflight_forced_to_delivered += 1;
    // ... emits WorkerPeerMessageDelivered ...
}
```

This means:
- The counter `inflight_forced_to_delivered` is incremented EVEN when the ledger reports `Unchanged` (no actual transition happened).
- A `WorkerPeerMessageDelivered` event is EMITTED when no actual delivery occurred.

This is the bug kimi flagged in bd-cpf.5 design review and asked to defer to bd-cpf.5c.

## Reachability analysis (today vs. Stage-2)

The reconciler matches `entry.state == LedgerState::DeliveredInflight` BEFORE calling the helper. The helper calls `ledger.transition(message_id, Delivered)`. `Ok(Unchanged)` happens iff the current ledger state already equals `Delivered`.

For this to fire, ANOTHER actor must have transitioned the entry from `DeliveredInflight` to `Delivered` between:
- `non_terminal_entries()` at line 41 (snapshot time), and
- `transition()` inside the helper (call time).

**Stage-1 reachability (today)**:
- `run_startup_reconcile` runs at orchestrator boot, BEFORE workers are dispatched (orchestrator.rs).
- The only actor that performs `DeliveredInflight → Delivered` is the orchestrator's post-prompt path (`orchestrator.rs:5005-5044`), which only runs while a worker is being prompted.
- Conclusion: **unreachable in normal Stage-1 boot flow**. Race is between snapshot and transition; no concurrent actor exists during reconciliation.

**Stage-2 reachability**:
- Persistent ledger may introduce eviction-and-reload cycles or recurring reconciliation.
- Crash-loop restart scenarios where reconciliation runs multiple times before the orchestrator stabilizes.
- Concurrent reconciliation tasks (if reconciler ever runs as a periodic task).
- Conclusion: **reachable in plausible Stage-2 scenarios**, particularly under crash-loops.

So this is a **dormant correctness bug**: harmless today, real before Stage-2 lands. Fixing it now is cheap insurance.

## The two errors caused by the bug (when reachable)

1. **Counter inflation**: `inflight_forced_to_delivered` over-counts. Operator dashboards reporting "messages forced to delivered by reconciler" become inaccurate.
2. **Spurious event emission**: `WorkerPeerMessageDelivered` fires for an entry the reconciler did NOT actually transition. Lineage projection treats this as a real delivery (it updates the peer-edge graph). Downstream consumers (TUI activity log, audit trails) see ghost delivery events.

The second error is more harmful — it pollutes the audit trail with events that don't reflect actual ledger transitions.

## Design questions

1. **Split-arm shape**:
   - **A1**: `Changed → emit + count`; `Unchanged(_) → debug log only`.
   - **A2**: `Changed → emit + count`; `Unchanged(_) → debug log + a NEW counter (e.g., `inflight_already_delivered`)`.

2. **Should the new "already-delivered" case emit any event?**
   - No event: silent debug log. Operators only see this via `WorkerPeerMailboxReconciled` summary if a counter is added.
   - New counter: tracking how often the race fires (Stage-2 visibility).
   - New diagnostic event: probably scope creep.

3. **Is the spurious `WorkerPeerMessageDelivered` event a CORRECTNESS issue or just observability noise?**
   - The event is consumed by `lineage/projection.rs` to update the peer-edge graph. If the entry is ALREADY in `Delivered` state, the lineage edge for this delivery already happened. Re-projecting the same edge is idempotent (set-like updates). So it's noise, not correctness corruption.
   - But: if downstream consumers count `WorkerPeerMessageDelivered` events (e.g., to compute "deliveries per restart"), they over-count.

4. **Counter shape: add `inflight_already_delivered`?**
   - Pro: distinct visibility into idempotent-race occurrences. Stage-2 needs this signal.
   - Con: yet another counter on `WorkerPeerMailboxReconciled`. The event already has 5 counters.
   - The `audit_failed_emitted` counter was introduced via bd-cpf.5b for a similar reason. Adding `inflight_already_delivered` is symmetrical.

5. **Should the doc-comment migration apply here too?**
   - If we add `inflight_already_delivered` as a new field, doc-comment it as "reflects races where another actor already advanced to Delivered."
   - If we don't add the counter, the bd-cpf.5c is purely about NOT-incrementing-on-Unchanged. No new field, no migration concern.

6. **Stage-1 risk of NOT adding the counter**: today the path is unreachable. If we don't add the counter, Stage-2 will need to add it. Doing it now is one less Stage-2 chore.

## Alternatives

### Alt A — Split arms, no new counter
```rust
TransitionAuditOutcome::Changed => {
    counts.inflight_forced_to_delivered += 1;
    // ... emit WorkerPeerMessageDelivered ...
}
TransitionAuditOutcome::Unchanged(state) => {
    tracing::debug!(
        message_id = ?entry.envelope.message_id,
        from = ?entry.state,
        observed_state = ?state,
        "reconciler: skipped emit — entry already at target state (concurrent advance)"
    );
}
TransitionAuditOutcome::TerminalSkip(state) => { ... }  // unchanged
TransitionAuditOutcome::AuditFailed(err) => { ... }      // unchanged
```

**Pro**: smallest patch (~10 LoC). Fixes the spurious event emission and counter inflation. Idempotent-race occurrences are silent in metrics but visible in debug logs.
**Con**: no Stage-2 visibility into how often the race fires.

### Alt B — Split arms, new `inflight_already_delivered` counter

```rust
pub struct ReconcileCounts {
    pub audit_failed_emitted: u32,
    pub inflight_forced_to_delivered: u32,
    pub inflight_stranded: u32,
    pub inflight_already_delivered: u32,   // NEW
    pub inflight_reverted_to_queued: u32,
    pub guards_re_wrapped: u32,
}

// Caller arms:
TransitionAuditOutcome::Changed => { /* count + emit */ }
TransitionAuditOutcome::Unchanged(state) => {
    counts.inflight_already_delivered += 1;
    tracing::debug!(...);
}
```

Plus the new field is added to `WorkerPeerMailboxReconciled`.

**Pro**: Stage-2 metric visibility. Symmetrical with bd-cpf.5b's `audit_failed_emitted` addition.
**Con**: +1 wire field + doc comment + serde round-trip test. ~20 LoC vs Alt A's ~10. New field defaults to 0 so wire-compat is fine.

### Alt C — Defer entirely

Since Stage-1 reachability is null, defer to Stage-2. Add a TODO comment.

**Pro**: zero patch. Zero risk.
**Con**: leaves a known correctness bug in the codebase. Reviewers (including kimi) flagged it in bd-cpf.5b — deferring twice signals it doesn't matter. It WILL fire in Stage-2.

## My provisional recommendation: Alt B

Reasons:
1. The fix is small and the additional counter is symmetrical with `audit_failed_emitted` (bd-cpf.5b).
2. Stage-2 will introduce the race; the counter pre-stages observability without a follow-up wire-shape change.
3. Symmetry with bd-cpf.5b's pattern: split arms + new counter + doc comment migration is a known shape; reviewers know what to evaluate.
4. Fixing now avoids leaving a correctness bug in the codebase.

Patch estimate: ~20 LoC reconciler + ~5 LoC events.rs (new field + doc comment) + ~5 LoC serde round-trip test + ~30 LoC test for the race scenario = ~60 LoC. Risk: low (helper already tested; reconciler arm split is mechanical).

## Asks for reviewers

1. Pick A/B/C (or your own D). Why?
2. Should the new `Unchanged` arm emit any event (e.g., a `WorkerPeerMessageReconcileNoOp`), or is debug-log silence the right shape?
3. Counter shape: add `inflight_already_delivered`, or is `audit_failed_emitted`'s pattern enough precedent to skip the symmetry?
4. Is the spurious `WorkerPeerMessageDelivered` event correctness-corrupting (lineage projection state) or just observability noise?
5. Patch size and risk for your preferred alternative.
