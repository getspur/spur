# bd-cpf.5b — reconciler `WorkerPeerMessageAuditFailed` emission gap: first-principles framing

## The bug

`crates/spur-core/src/peer_mailbox/reconciler.rs:81-89` logs a `tracing::warn!` when the startup-reconcile transition `DeliveredInflight → Delivered` fails:

```rust
Err(err) => {
    tracing::warn!(
        message_id = ?entry.envelope.message_id,
        from = ?entry.state,
        to = ?LedgerState::Delivered,
        ?err,
        "peer mailbox startup reconcile transition failed"
    );
}
```

But it does NOT:
1. Emit `WorkerPeerMessageAuditFailed` to the funnel.
2. Increment `counts.audit_failed_emitted`.

The `ReconcileCounts.audit_failed_emitted` field exists and is propagated into `WorkerPeerMailboxReconciled.audit_failed_emitted`, but it is **always 0** under all reachable paths today. Operators dashboards on this field would always show "no anomalies" even when the reconciler is silently failing.

This was discovered by kimi during the bd-cpf.5 design review and explicitly deferred to bd-cpf.5b as a behavioral change (refactor ≠ behavioral change).

## Reachability

When can this actually fire? The reconciler attempts `DeliveredInflight → Delivered` only when:
- `entry.state == LedgerState::DeliveredInflight` AND
- `!entry.injected_into_prompts.is_empty()` (otherwise the message is "stranded" per bd-cpf.3).

Possible failure modes:
- `LedgerError::NotFound` — impossible today (in-memory ledger; entry was just iterated).
- `LedgerError::AlreadyTerminal` — impossible (`DeliveredInflight` is non-terminal).
- `LedgerError::InvalidTransition` — possible if some other concurrent code transitions the entry between `non_terminal_entries()` and `transition()`. Unlikely under current locking but reachable.
- Stage-2 (persistent ledger): I/O errors, eviction races, schema mismatches → all reachable.

So it's a low-probability path today, but it WILL be reachable in Stage-2. The audit must work before then or operators get false silence on real failures.

## Behavioral change vs refactor

This IS a behavioral change:
- `WorkerPeerMailboxReconciled.audit_failed_emitted` moves from always-0 to sometimes-non-zero.
- A new `WorkerPeerMessageAuditFailed` event will appear in the funnel under specific conditions (none-today, some-Stage-2).

Anyone who:
- Filters dashboards on `audit_failed_emitted == 0` will now see anomalies.
- Aggregates `WorkerPeerMessageAuditFailed` events will see a new source.

That's why bd-cpf.5 (refactor) deliberately did NOT touch this. bd-cpf.5b is the right home.

## Design questions

1. **Reuse the bd-cpf.5 helper or inline emit?**
   - Reuse `transition_with_audit(...)` — pulls reconciler into the same audit codepath as post-prompt. Brings reconciler into the bd-cpf.5 abstraction.
   - Inline emit — minimal change, reconciler stays self-contained.

2. **What `transition_kind` string should the AuditFailed event carry?**
   - Reuse `"delivered"` (matches post-prompt's second transition; simpler dashboards).
   - New value `"reconcile_to_delivered"` (distinguishes startup-reconcile failure from post-prompt failure for alerting).

3. **What about the `from/to` log fields?**
   - The current `tracing::warn!` carries `from = ?entry.state` and `to = ?LedgerState::Delivered`. These don't appear in `AuditFailed` events.
   - Helper-based: helper would emit AuditFailed with just `transition_kind`; caller log keeps `from/to`. OK.
   - Inline: caller does both — same observability.

4. **Should the `Ok(_)` arm also use the helper?**
   - The helper distinguishes Changed from Unchanged. The reconciler's current `Ok(_)` arm collapses both. If we use the helper, we'd map `Changed` to "emit Delivered + count", `Unchanged` to "no-op, no count" (or "count differently?").
   - Today's behavior: ANY `Ok` increments `inflight_forced_to_delivered`. Idempotent re-reconcile would double-count. This is arguably ANOTHER bug — but it's out of scope for bd-cpf.5b (which is specifically about audit emission).

5. **What if the `Changed` outcome is needed for the count, but `Unchanged` shouldn't be counted?**
   - This forces us to fork. But again — out of scope; today's logic counts both.

6. **Do we want a new `PeerTransitionKind::ReconcileToDelivered` variant?**
   - Pro: cardinality-bounded (1 new value), gives ops a distinct signal.
   - Con: changes the `PeerTransitionKind` API just-merged in bd-cpf.5; the helper has to know about it.

7. **Test coverage**: need a test that triggers the failure path. Mock ledger that returns `Err(LedgerError::InvalidTransition)` from `transition()`?

## Alternatives

### Alt A — Inline emit, reuse `"delivered"` kind

```rust
Err(err) => {
    tracing::warn!(
        message_id = ?entry.envelope.message_id,
        from = ?entry.state,
        to = ?LedgerState::Delivered,
        ?err,
        "peer mailbox startup reconcile transition failed"
    );
    funnel.emit(SpurEventBody::WorkerPeerMessageAuditFailed {
        brain_session_id: brain_session_id.clone(),
        message_id: entry.envelope.message_id,
        target_delegation_id: entry.envelope.target_delegation_id.clone(),
        transition_kind: "delivered".into(),
        error: err.to_string(),
    });
    counts.audit_failed_emitted += 1;
}
```

**Pro**: smallest diff (~10 LoC). No new abstraction. Easy to review.
**Con**: third audit-failed-emit site (after post-prompt × 2 in bd-cpf.5). Re-introduces the very duplication bd-cpf.5 just removed.

### Alt B — Use `transition_with_audit` helper, reuse `"delivered"` kind

Replace the entire `match ledger.transition(...)` block with a `transition_with_audit` call. Map outcomes:
- `Changed` → emit `WorkerPeerMessageDelivered` + `counts.inflight_forced_to_delivered += 1`.
- `Unchanged(state)` → debug log "no-op", count? (today's logic counts; preserve.)
- `TerminalSkip(state)` → impossible from `DeliveredInflight`-non-terminal source; debug log + skip.
- `AuditFailed(err)` → caller already-emitted. Caller does its own `tracing::warn!` with from/to/err + `counts.audit_failed_emitted += 1`.

**Pro**: unifies all 3 audit sites under one abstraction. Future Stage-2 errors handled in helper. No new code duplication.
**Con**: helper signature is post-prompt-shaped (`PeerTransitionKind::Delivered` is the closest match; `"delivered"` is the wire string). Reconciler caller has to handle `Changed` differently than post-prompt (different event payload — `injected_chars: 0` distortion). Slightly more code at the caller than today.

### Alt C — Use helper, add new `PeerTransitionKind::ReconcileToDelivered` variant

Same as Alt B, but the `transition_kind` is `"reconcile_to_delivered"` (new variant), so dashboards can distinguish startup-reconcile audit failures from post-prompt audit failures.

**Pro**: operationally cleaner alerting. New cardinality value is bounded (just one).
**Con**: API change to bd-cpf.5's just-merged enum. Adds a new wire string consumers might not know about. Could be deferred to Stage-2.

## My provisional recommendation: Alt B (use helper, reuse `"delivered"`)

Reasons:
1. Avoids re-introducing the duplication bd-cpf.5 removed.
2. `"delivered"` wire string is the same transition target — semantically consistent.
3. The "post-prompt vs reconcile" distinction can be inferred from co-occurring events: an `AuditFailed { transition_kind: "delivered" }` near a `WorkerPeerMailboxReconciled` is a reconcile failure; one in isolation during normal operation is post-prompt. Operators don't strictly need a new wire string.
4. Helper handles Stage-2 error variants centrally.

But Alt C is a defensible alternative if reviewers think the operational distinction is worth the new cardinality.

Patch size estimate (Alt B): ~30 LoC change to reconciler.rs + ~15 LoC test = ~45 LoC. Risk: low (helper is already tested in bd-cpf.5).

## Asks for reviewers

1. Pick A/B/C (or your own D). Why?
2. `transition_kind` reuse vs new variant — what does ops want?
3. Should `audit_failed_emitted += 1` be the ONLY count change, or do we also fix the Ok-double-count bug noted above? (Strong argument: defer the double-count fix to a separate ticket.)
4. Test coverage: how to deterministically trigger `LedgerError` in `transition()`?
5. Patch size and risk for your preferred alternative.
