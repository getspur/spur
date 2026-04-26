# bd-cpf.5b design synthesis — Alt C (helper reuse + new `ReconcileToDelivered` variant)

After 3-round L9 sequential-thinking MCTS over the three design reviews, the chosen design is **Alt C** with all of kimi's operational amendments plus codex's `TerminalSkip`-policy clarification.

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Alternative | **Alt B** (`"delivered"`) | Alt C | Alt C | **Alt C** | gemini |
| Helper reuse vs inline | helper | helper | helper | **helper** | converged |
| TerminalSkip policy | (silent) | (silent) | **flag as SHOULD-FIX** | **silent — document as "benign race"** | converged after codex flag |
| Doc-comment migration on `audit_failed_emitted` | (no) | **BLOCKER** | (no) | **yes** | kimi-specific |
| Test approach | mock ledger | `FaultInjectionLedger` wrapper | mock ledger | **`FaultInjectionLedger`** | converged |
| Ok-double-count bug | out of scope | bd-cpf.5c | out of scope | **bd-cpf.5c (separate)** | converged |
| `from`/`to` log fields | preserve | preserve | preserve | **preserve at call site** | converged |
| Make enum `#[non_exhaustive]` | NIT | (no) | (no) | **defer (NIT only)** | follow gemini |

## Override rationale

1. **gemini's Alt B → Alt C**: 2-1 vote (kimi + codex) for Alt C with strong operational reasoning. Kimi's P1-vs-P2 distinction is decisive: post-prompt audit failure means "worker bug" (P2); reconcile audit failure means "data corruption on boot" (P1). Distinct alert routes need distinct wire strings. Codex agrees: "distinct bounded-cardinality audit string lets dashboards distinguish without inferring from nearby events." Cardinality cost is exactly 1 new value; `transition_kind` is already `String` on the wire (no schema change). Alt C is operationally correct and technically cheap.

2. **TerminalSkip silent (codex SHOULD-FIX resolved)**: Codex flagged that helper-based reuse silences the `Err(InvalidTransition)` arm when `is_terminal(from)` — this changes today's behavior where the reconciler warns on every Err. The right policy is **silent**: a benign race where another actor (worker ack, drain timeout, post-prompt path) terminalizes the message between `non_terminal_entries()` snapshot and the reconciler's `transition()` call should NOT page operators. AuditFailed is reserved for non-terminal errors indicating ledger inconsistency. Document this explicitly in the synthesis doc and via a code comment.

3. **Doc-comment migration (kimi BLOCKER)**: `audit_failed_emitted` going from always-0 to sometimes-non-zero is a behavioral change that any dashboard filtering on `== 0` will see. Doc-comment migration on both `ReconcileCounts.audit_failed_emitted` (Rust struct) and `WorkerPeerMailboxReconciled.audit_failed_emitted` (event struct) costs ~5 LoC and prevents operator confusion.

4. **Ok-double-count bug → bd-cpf.5c**: All three converge. The reconciler's `Ok(_) => counts.inflight_forced_to_delivered += 1` collapses Changed and Unchanged, double-counting on idempotent re-reconcile. Real bug, but out of scope for an audit-emission fix. File separately.

## The Alt C design

### Module change: `crates/spur-core/src/peer_mailbox/transitions.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTransitionKind {
    DeliveredInflight,
    Delivered,
    /// Startup reconciliation forcing a `DeliveredInflight` entry to
    /// `Delivered`. Distinct from `Delivered` (post-prompt) so audit
    /// failures route to a different alert tier — reconcile audit
    /// failures indicate boot-time ledger inconsistency.
    ReconcileToDelivered,
}

impl PeerTransitionKind {
    pub fn as_audit_str(&self) -> &'static str {
        match self {
            Self::DeliveredInflight => "delivered_inflight",
            Self::Delivered => "delivered",
            Self::ReconcileToDelivered => "reconcile_to_delivered",
        }
    }
}
```

### Reconciler change: `crates/spur-core/src/peer_mailbox/reconciler.rs`

The current `match ledger.transition(&entry.envelope.message_id, LedgerState::Delivered).await` block is replaced with:

```rust
use crate::peer_mailbox::{
    transition_with_audit, PeerTransitionKind, TransitionAuditOutcome,
};

// Before the loop (since brain_session_id is currently `String`):
let brain_session_id_typed =
    spur_acp::BrainSessionId::new(spur_acp::types::SessionId(brain_session_id.clone()));

// Inside the DeliveredInflight + non-empty-injection arm:
match transition_with_audit(
    ledger.as_ref(),
    &funnel,
    &brain_session_id_typed,
    &entry.envelope.target_delegation_id,
    entry.envelope.message_id,
    LedgerState::Delivered,
    PeerTransitionKind::ReconcileToDelivered,
)
.await
{
    TransitionAuditOutcome::Changed | TransitionAuditOutcome::Unchanged(_) => {
        counts.inflight_forced_to_delivered += 1;
        let target_prompt_id = entry
            .injected_into_prompts
            .iter()
            .min()
            .cloned()
            .unwrap_or_default();
        // `injected_chars: 0` distortion preserved (bd-cpf.3 comment)
        funnel.emit(SpurEventBody::WorkerPeerMessageDelivered {
            brain_session_id: brain_session_id.clone(),
            message_id: entry.envelope.message_id,
            target_delegation_id: entry.envelope.target_delegation_id.clone(),
            target_prompt_id,
            injected_chars: 0,
        });
    }
    TransitionAuditOutcome::TerminalSkip(state) => {
        // Benign race: another actor (worker ack, drain, post-prompt)
        // terminalized the message between `non_terminal_entries()` and
        // this transition. Not an audit failure; just a resolved race.
        tracing::debug!(
            message_id = ?entry.envelope.message_id,
            from = ?entry.state,
            terminal_state = ?state,
            "reconciler: transition skipped because message reached terminal state via concurrent actor"
        );
    }
    TransitionAuditOutcome::AuditFailed(err) => {
        tracing::warn!(
            message_id = ?entry.envelope.message_id,
            from = ?entry.state,
            to = ?LedgerState::Delivered,
            %err,
            "peer mailbox startup reconcile transition failed"
        );
        counts.audit_failed_emitted += 1;
    }
}
```

Note: the **`Changed | Unchanged(_)` arm preserves today's collapse**. The double-count bug (bd-cpf.5c) is explicitly NOT addressed here.

### Doc-comment migration

In `crates/spur-core/src/peer_mailbox/reconciler.rs::ReconcileCounts`:
```rust
pub struct ReconcileCounts {
    /// Count of `WorkerPeerMessageAuditFailed` events emitted during
    /// reconciliation.
    ///
    /// Migration note: prior to bd-cpf.5b, this field was always 0
    /// because the reconciler did not emit `WorkerPeerMessageAuditFailed`
    /// on transition errors. After bd-cpf.5b it reflects the real count.
    /// Dashboards filtering on `== 0` should switch to alerting on the
    /// `WorkerPeerMessageAuditFailed` event type with
    /// `transition_kind == "reconcile_to_delivered"` instead.
    pub audit_failed_emitted: u32,
    // ... other fields unchanged
}
```

In `crates/spur-acp/src/domain/events.rs::SpurEventBody::WorkerPeerMailboxReconciled`:
```rust
WorkerPeerMailboxReconciled {
    // ... existing fields with their existing doc comments preserved
    /// Count of `WorkerPeerMessageAuditFailed` events emitted during
    /// reconciliation. Always 0 prior to bd-cpf.5b. Use the
    /// `WorkerPeerMessageAuditFailed` event type (filtered by
    /// `transition_kind == "reconcile_to_delivered"`) for direct alerting
    /// rather than this counter.
    audit_failed_emitted: u32,
    // ...
}
```

### Test: `FaultInjectionLedger`

Use the wrapper pattern from kimi's review:

```rust
struct FaultInjectionLedger {
    inner: Arc<InMemoryLedger>,
    fail_for: tokio::sync::Mutex<HashSet<PeerMessageId>>,
}

#[async_trait::async_trait]
impl PeerMailboxLedger for FaultInjectionLedger {
    async fn transition(&self, message_id: &PeerMessageId, next: LedgerState) -> Result<TransitionOutcome, LedgerError> {
        if self.fail_for.lock().await.contains(message_id) {
            return Err(LedgerError::InvalidTransition {
                from: LedgerState::Queued,  // non-terminal, so it triggers AuditFailed
                to: next,
            });
        }
        self.inner.transition(message_id, next).await
    }
    // ... delegate other methods to inner
}
```

Test name: `reconcile_emits_audit_failed_when_transition_fails_non_terminally`. Asserts:
- `counts.audit_failed_emitted == 1`
- exactly one `WorkerPeerMessageAuditFailed { transition_kind: "reconcile_to_delivered", error, .. }` event with non-empty `error`
- `WorkerPeerMailboxReconciled` event carries `audit_failed_emitted: 1`

## What this preserves

- Existing reconciler tests pass unchanged (success path, stranded path, mixed path).
- `from`/`to` log fields preserved at call site (kimi BLOCKER from bd-cpf.5).
- `injected_chars: 0` distortion preserved (bd-cpf.3 comment).
- `inflight_forced_to_delivered` counter still increments on Ok (Changed AND Unchanged collapse — bd-cpf.5c will fix).
- `Stranded` path untouched; `guards_re_wrapped` untouched.

## Patch estimate

| File | LoC |
|---|---|
| `peer_mailbox/transitions.rs` | +3 (variant + as_audit_str arm) |
| `peer_mailbox/reconciler.rs` | net +20 (replace match, add migration doc) |
| `spur-acp/domain/events.rs` | +5 (doc-comment migration) |
| New test (mock ledger + scenario) | +50 |
| **Total** | **~+78 LoC** |

Risk: **low**. The helper is already tested in bd-cpf.5; this PR adds a single new enum variant and routes one new caller.

## Followups (NOT bd-cpf.5b scope)

| Item | Tracking |
|---|---|
| Reconciler `Ok(_)` double-count (Changed + Unchanged collapse) | **bd-cpf.5c** |
| `PeerTransitionKind` `#[non_exhaustive]` consideration | NIT (defer) |
| `WorkerPeerMailboxReconciled` `reconciled_at` timestamp | Stage-2 (kimi open Q) |
| Stage-2 phantom-terminal scenarios (if persistent ledger creates them, may need a different signal) | Stage-2 |
