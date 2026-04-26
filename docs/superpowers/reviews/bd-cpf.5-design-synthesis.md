# bd-cpf.5 design synthesis — Alt E'' (typed-return helper, 2 sites, audit-not-logs)

After 5-round L9 sequential-thinking MCTS over the three design reviews (gemini Alt E + 3 sites + helper-owns-logs, kimi Alt A/B + 2 sites + BLOCKERs on log strings, codex Alt E + helper takes ledger trait + typed return), the chosen design is **Alt E''**: a typed-return helper at `peer_mailbox/transitions.rs` that owns ledger transition + AuditFailed emission + outcome classification, but explicitly does NOT own tracing logs (those stay at call sites to preserve exact strings).

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Site scope | 3 (incl reconciler) | **2** (BLOCKER) | 2 or 3 with caveat | **2** | gemini |
| Helper owns logs? | yes | **no** (BLOCKER) | audit only | **no** | gemini |
| `transition_kind` shape | string | **typed enum** | string (defer enum) | **typed enum** | codex |
| Helper location | `peer_mailbox/audit.rs` | (silent) | `peer_mailbox/transitions.rs` | **`transitions.rs`** | converged |
| Outcome enum | `TransitionAuditOutcome` (4 variants) | `TransitionRecordOutcome` | `TransitionAuditOutcome` | **`TransitionAuditOutcome`** | name converge |
| Cascade preservation | implicit | **BLOCKER** explicit | preserve | **preserve** | converged |
| Reconciler `AuditFailed` gap | fold in (fix bug) | ticket separately | flag explicitly | **separate ticket (bd-cpf.5b)** | follow kimi |
| Closure-based Alt A | reject | reject | reject | **rejected** | converged |
| Helper ledger arg | `&PeerMailboxLedger` | (silent) | `&dyn PeerMailboxLedger` | **`&dyn PeerMailboxLedger`** | follow codex |

## Override rationale

1. **gemini's "fold reconciler" → kimi's "2 sites only"**: kimi is operationally rigorous. The reconciler's `audit_failed_emitted` field is permanently 0 today (`reconciler.rs:81-89` warns but never emits `WorkerPeerMessageAuditFailed`). Folding it into a helper that emits AuditFailed silently changes that — `audit_failed_emitted` would move from always-0 to sometimes-non-zero, breaking any dashboard or alert that relies on the current shape. Pure refactors must preserve all observable signals. The gap IS a bug, but bd-cpf.5's mandate is dedup, not bug-fixing. File the reconciler gap as **bd-cpf.5b**: a behavioral change with its own tests, rollback, and review cycle.

2. **gemini's "helper owns logs" → kimi's "helper does NOT own logs"**: kimi's BLOCKER on Loki/Grafana queries is operationally rigorous. Today's exact log strings (kimi's table of 7) are operational contracts. A helper that interpolates a generic template would change "delivered-inflight" (hyphen) to "delivered_inflight" (underscore), or `"post-prompt DeliveredInflight transition skipped..."` to `"post-prompt {Kind} transition skipped..."`, breaking exact-string queries. Keeping `tracing::debug!` and `tracing::warn!` calls at call sites preserves them by construction. The helper gets smaller but its purpose (consolidating the AuditFailed-on-error logic + outcome classification) is preserved.

3. **codex's "string transition_kind" (defer enum) → kimi's "typed enum NOW"**: kimi's typo-proofing argument is cheap and right. Adding `PeerTransitionKind` enum with `as_audit_str() -> &'static str` is ~10 LoC and prevents future bugs. The wire format stays `String` (event type unchanged); only the helper's input is typed. Stage-2 can extend the enum without touching call sites.

## The Alt E'' design

### Module: `crates/spur-core/src/peer_mailbox/transitions.rs`

```rust
use std::sync::Arc;

use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageId};
use spur_acp::{BrainSessionId, SpurEventBody};

use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::{is_terminal, LedgerError, PeerMailboxLedger, TransitionOutcome};

/// Categorizes which post-prompt / reconciler transition is being attempted.
/// Used to populate `WorkerPeerMessageAuditFailed.transition_kind` with a
/// typo-proof string, and to keep the wire format stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTransitionKind {
    DeliveredInflight,
    Delivered,
    // Stage-2 may add: Persisted, Evicted, etc.
}

impl PeerTransitionKind {
    pub fn as_audit_str(&self) -> &'static str {
        match self {
            Self::DeliveredInflight => "delivered_inflight",
            Self::Delivered => "delivered",
        }
    }
}

/// Result of a `transition_with_audit` call. Caller maps to its
/// site-specific tracing logs and Changed-side-effects.
#[derive(Debug)]
pub enum TransitionAuditOutcome {
    /// `Ok(Changed)` — caller emits site-specific event (e.g., Delivered).
    Changed,
    /// `Ok(Unchanged)` — caller may log a debug "no-op" message with the
    /// returned current state.
    Unchanged(LedgerState),
    /// `Err(InvalidTransition)` where `is_terminal(from)` — caller logs a
    /// debug "skipped: already terminal" and SHOULD `continue` the outer
    /// loop to skip subsequent transitions for this message.
    TerminalSkip(LedgerState),
    /// Other `Err` — helper has already emitted `WorkerPeerMessageAuditFailed`.
    /// Caller logs a `tracing::warn!` and falls through to the next transition
    /// (preserving today's cascade behavior).
    AuditFailed,
}

/// Attempt a ledger transition for a peer message and emit
/// `WorkerPeerMessageAuditFailed` on non-terminal errors.
///
/// Tracing logs (debug for no-op / terminal-skip; warn for AuditFailed) are
/// the caller's responsibility — they are intentionally NOT homogenized into
/// the helper, because exact log strings are an operational contract for
/// log-aggregation queries.
///
/// Cascade behavior is preserved: a non-terminal failure on the first
/// transition does NOT short-circuit the second; the caller falls through
/// after handling `AuditFailed`.
pub async fn transition_with_audit(
    ledger: &dyn PeerMailboxLedger,
    funnel: &FunnelHandle,
    brain_session_id: &BrainSessionId,
    target_delegation_id: &DelegationId,
    message_id: PeerMessageId,
    target_state: LedgerState,
    transition_kind: PeerTransitionKind,
) -> TransitionAuditOutcome {
    match ledger.transition(&message_id, target_state).await {
        Ok(TransitionOutcome::Changed { .. }) => TransitionAuditOutcome::Changed,
        Ok(TransitionOutcome::Unchanged(state)) => TransitionAuditOutcome::Unchanged(state),
        Err(LedgerError::InvalidTransition { from, .. }) if is_terminal(from) => {
            TransitionAuditOutcome::TerminalSkip(from)
        }
        Err(err) => {
            funnel.emit(SpurEventBody::WorkerPeerMessageAuditFailed {
                brain_session_id: brain_session_id.to_string(),
                message_id,
                target_delegation_id: target_delegation_id.clone(),
                transition_kind: transition_kind.as_audit_str().to_string(),
                error: err.to_string(),
            });
            TransitionAuditOutcome::AuditFailed
        }
    }
}
```

### Caller pattern at `orchestrator.rs:4968-5059`

```rust
use crate::peer_mailbox::transitions::{
    transition_with_audit, PeerTransitionKind, TransitionAuditOutcome,
};

for inj in pc.injection_records {
    match transition_with_audit(
        bundle.ledger.as_ref(),
        &funnel,
        &ctx.brain_session_id,
        &target_delegation_id,
        inj.message_id,
        LedgerState::DeliveredInflight,
        PeerTransitionKind::DeliveredInflight,
    )
    .await
    {
        TransitionAuditOutcome::Changed => {}
        TransitionAuditOutcome::Unchanged(state) => {
            tracing::debug!(
                message_id = ?inj.message_id,
                state = ?state,
                "peer mailbox: delivered-inflight transition no-op"
            );
        }
        TransitionAuditOutcome::TerminalSkip(state) => {
            tracing::debug!(
                message_id = ?inj.message_id,
                state = ?state,
                "post-prompt DeliveredInflight transition skipped: message already terminal"
            );
            continue;
        }
        TransitionAuditOutcome::AuditFailed => {
            tracing::warn!(
                message_id = ?inj.message_id,
                "peer mailbox: delivered-inflight transition failed"
            );
        }
    }

    match transition_with_audit(
        bundle.ledger.as_ref(),
        &funnel,
        &ctx.brain_session_id,
        &target_delegation_id,
        inj.message_id,
        LedgerState::Delivered,
        PeerTransitionKind::Delivered,
    )
    .await
    {
        TransitionAuditOutcome::Changed => {
            funnel.emit(SpurEventBody::WorkerPeerMessageDelivered {
                brain_session_id: ctx.brain_session_id.to_string(),
                message_id: inj.message_id,
                target_delegation_id: target_delegation_id.clone(),
                target_prompt_id: pc.target_prompt_id.clone(),
                injected_chars: inj.injected_bytes,
            });
        }
        TransitionAuditOutcome::Unchanged(state) => {
            tracing::debug!(
                message_id = ?inj.message_id,
                state = ?state,
                "peer mailbox: delivered transition no-op"
            );
        }
        TransitionAuditOutcome::TerminalSkip(state) => {
            tracing::debug!(
                message_id = ?inj.message_id,
                state = ?state,
                "post-prompt Delivered transition skipped: message already terminal"
            );
            continue;
        }
        TransitionAuditOutcome::AuditFailed => {
            tracing::warn!(
                message_id = ?inj.message_id,
                "peer mailbox: delivered transition failed"
            );
        }
    }
}
```

### Module export at `peer_mailbox/mod.rs`

```rust
pub mod transitions;
pub use transitions::{
    transition_with_audit, PeerTransitionKind, TransitionAuditOutcome,
};
```

### Tests

In `transitions.rs::tests` mod (use `InMemoryLedger` + `event_funnel::test_channel`):

1. `transition_with_audit_returns_changed_on_normal_path` — accept envelope, transition Accepted→DeliveredInflight, assert `Changed`, no AuditFailed event.
2. `transition_with_audit_returns_unchanged_on_idempotent_target` — same target as current, assert `Unchanged(state)`, no AuditFailed event.
3. `transition_with_audit_returns_terminal_skip_when_already_terminal` — force ledger to terminal (`Consumed`), attempt transition to Delivered, assert `TerminalSkip(Consumed)`, no AuditFailed event.
4. `transition_with_audit_emits_audit_failed_on_invalid_non_terminal_transition` — attempt invalid transition (e.g., Queued→Delivered without inflight), assert `AuditFailed`, exactly one `WorkerPeerMessageAuditFailed` event with correct `transition_kind` string.

Existing post-prompt integration tests (in orchestrator.rs and integration tests) should pass unchanged — cascade behavior and exact log strings preserved.

## What this preserves (kimi's pager assertions)

1. **`AuditFailed.transition_kind` strings stable**: `PeerTransitionKind::as_audit_str()` returns `"delivered_inflight"` and `"delivered"` exactly. ✓
2. **Terminal-skip path silent at event level**: `TerminalSkip` outcome means helper does NOT emit AuditFailed; caller's debug log is at call site (exact string preserved). ✓
3. **Cascade behavior preserved**: `AuditFailed` outcome means caller falls through to the next transition; second transition emits its own `AuditFailed` (cascade noise is the current behavior, not a bug to fix here). ✓
4. **Exact log strings**: all 4 `tracing::debug!` and 2 `tracing::warn!` strings preserved verbatim at call sites. ✓
5. **Event order**: helper emits `AuditFailed` before returning, so caller's `warn!` runs immediately after — minor sub-millisecond reordering vs today's `warn!` then `emit`. Not contractual.

## Patch estimate

| File | LoC |
|---|---|
| `peer_mailbox/transitions.rs` (new) | +80 (types + helper + 4 tests) |
| `peer_mailbox/mod.rs` | +3 (module export) |
| `orchestrator.rs:4968-5059` | net -60 (replace 90 LoC of duplicated arms with 30 LoC of outcome matches) |
| **Total** | **~+25 LoC net (with tests)** |

Risk: **low**. The helper preserves cascade, exact log strings, exact `transition_kind` strings, and emit ordering (within microseconds). Reconciler is untouched.

## Followups (NOT bd-cpf.5 scope)

| Item | Tracking |
|---|---|
| Reconciler `WorkerPeerMessageAuditFailed` emission gap (`reconciler.rs:81-89`) | **bd-cpf.5b** (separate ticket) |
| Helper extension for Stage-2 `LedgerError::Eviction` / `Persistence` | Stage-2 |
| Optional `WorkerPeerMessageTransitionRecorded` diagnostic event | Stage-2 |
| Cascade noise (false-positive 2nd AuditFailed when 1st fails non-terminally) | Pre-existing; document as known behavior |
| TUI activity-log mapping for any new events | Stage-2 |
