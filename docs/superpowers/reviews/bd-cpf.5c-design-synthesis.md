# bd-cpf.5c design synthesis — Alt D (split arms + new counter + serde-default cleanup)

After 3-round L9 sequential-thinking MCTS over the three design reviews and a code-evidence verification, the chosen design is **Alt D** = Alt B (split arms + new `inflight_already_delivered` counter) plus codex's compatibility cleanup (add missing `#[serde(default)]` to `audit_failed_emitted`).

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Alternative | Alt B | Alt B | **Alt D** | **Alt D** | follow codex |
| Lineage `injected_chars` clobber | **BLOCKER** | NIT | **BLOCKER** | **BLOCKER** | override kimi (code-verified) |
| Doc-comment shape | (paste migration) | **plain doc** | (silent) | **plain doc** | follow kimi |
| `#[serde(default)]` on new field | required | implicit | required | **required** | converged |
| Fix bd-cpf.5b's missing `serde(default)` on `audit_failed_emitted` | (silent) | (silent) | propose | **yes (Alt D)** | follow codex |
| Backward-compat replay test | (silent) | (silent) | suggest | **add** | follow codex |
| Field placement near `inflight_forced_to_delivered` | (silent) | (silent) | NIT | **yes** | follow codex |
| Mock ledger pattern | `RaceLedger` | `RaceSimulatingLedger` | mock w/ consistent get() | **`RaceSimulatingLedger`** | converged shape |
| CHANGELOG entry | (silent) | NIT (under Added) | (silent) | **add** | follow kimi |

## Override rationale

1. **kimi's "lineage NIT" → BLOCKER**. Direct code verification at `crates/spur-core/src/lineage/projection.rs:589-591`:
   ```rust
   if let Some(injected_chars) = injected_chars {
       edge.injected_chars = injected_chars;
   }
   ```
   The reconciler's `WorkerPeerMessageDelivered { injected_chars: 0 }` calls `update_peer_edge_state(..., Some(0))` — unconditionally overwriting `edge.injected_chars`. If the orchestrator's post-prompt path emitted a real byte count first (race-loss scenario), the reconciler's spurious event clobbers it to 0. **This is correctness-corrupting, not noise.** Gemini + codex got this right (2-1 vote with code evidence).

2. **kimi's "plain-doc, not migration"**. bd-cpf.5b's migration note was for an existing field whose semantics changed (always-0 → sometimes-non-zero). `inflight_already_delivered` is brand new — no prior dashboards rely on it. A "migration note" would be misleading ("migration from what?"). Use plain documentation.

3. **codex's Alt D compatibility cleanup**. `audit_failed_emitted` (added by bd-cpf.5b) lacks `#[serde(default)]` — this was an oversight. Replays loading events from JSONL fixtures predating the field would fail to deserialize. Same review surface, low marginal cost. Apply.

## The Alt D design

### Reconciler arm split (`reconciler.rs:76`)

```rust
TransitionAuditOutcome::Changed => {
    counts.inflight_forced_to_delivered += 1;
    let target_prompt_id = entry
        .injected_into_prompts
        .iter()
        .min()
        .cloned()
        .unwrap_or_default();
    funnel.emit(SpurEventBody::WorkerPeerMessageDelivered {
        brain_session_id: brain_session_id.clone(),
        message_id: entry.envelope.message_id,
        target_delegation_id: entry.envelope.target_delegation_id.clone(),
        target_prompt_id,
        injected_chars: 0,    // bd-cpf.3 distortion comment preserved
    });
}
TransitionAuditOutcome::Unchanged(state) => {
    // Benign race: another actor (post-prompt path, concurrent reconcile)
    // advanced the entry to Delivered between `non_terminal_entries()`
    // snapshot and this transition. Do NOT emit `WorkerPeerMessageDelivered`
    // — the lineage projection would otherwise clobber the real
    // `injected_chars` value with our placeholder 0.
    counts.inflight_already_delivered += 1;
    tracing::debug!(
        message_id = ?entry.envelope.message_id,
        from = ?entry.state,
        observed_state = ?state,
        "reconciler: skipped emit — entry already at target state (concurrent advance)"
    );
}
TransitionAuditOutcome::TerminalSkip(state) => { /* unchanged from bd-cpf.5b */ }
TransitionAuditOutcome::AuditFailed(err) => { /* unchanged from bd-cpf.5b */ }
```

### `ReconcileCounts` (in `reconciler.rs`)

```rust
pub struct ReconcileCounts {
    /// (existing doc) ...
    pub audit_failed_emitted: u32,
    pub inflight_forced_to_delivered: u32,
    /// Count of reconciler entries that were already in `Delivered` state
    /// when the reconciler attempted to force them there. This reflects
    /// benign races where another actor (post-prompt path, concurrent
    /// reconcile) advanced the state between `non_terminal_entries()`
    /// snapshot and the transition call.
    /// Stage-1: always 0 (no concurrent actor). Stage-2: expected non-zero
    /// under crash-loop or periodic-reconcile scenarios.
    pub inflight_already_delivered: u32,
    pub inflight_stranded: u32,
    pub inflight_reverted_to_queued: u32,
    pub guards_re_wrapped: u32,
}
```

### `WorkerPeerMailboxReconciled` event variant (in `events.rs`)

```rust
WorkerPeerMailboxReconciled {
    brain_session_id: String,
    /// (existing doc preserved verbatim — bd-cpf.5b migration note)
    #[serde(default)]                  // NEW: codex Alt D compat fix
    audit_failed_emitted: u32,
    inflight_forced_to_delivered: u32,
    /// Count of reconciler entries already in `Delivered` state at
    /// transition time (benign concurrent-advance races). See
    /// `ReconcileCounts::inflight_already_delivered`.
    #[serde(default)]
    inflight_already_delivered: u32,
    #[serde(default)]
    inflight_stranded: u32,
    inflight_reverted_to_queued: u32,
    guards_re_wrapped: u32,
},
```

Field order: place `inflight_already_delivered` immediately after `inflight_forced_to_delivered` (codex NIT — the no-op twin).

### Test: `RaceSimulatingLedger`

```rust
struct RaceSimulatingLedger {
    inner: Arc<InMemoryLedger>,
    race_for: tokio::sync::Mutex<HashSet<PeerMessageId>>,
}

#[async_trait::async_trait]
impl PeerMailboxLedger for RaceSimulatingLedger {
    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<TransitionOutcome, LedgerError> {
        if next == LedgerState::Delivered && self.race_for.lock().await.contains(message_id) {
            // Simulate a concurrent actor that already advanced the entry.
            return Ok(TransitionOutcome::Unchanged(LedgerState::Delivered));
        }
        self.inner.transition(message_id, next).await
    }
    // ... delegate other methods to inner
}
```

Test: `reconcile_increments_already_delivered_when_race_observes_unchanged`. Asserts:
- `counts.inflight_forced_to_delivered == 0`
- `counts.inflight_already_delivered == 1`
- exactly ONE `WorkerPeerMailboxReconciled` event emitted (no `WorkerPeerMessageDelivered`)
- `WorkerPeerMailboxReconciled.inflight_already_delivered == 1`

### Backward-compat replay test (codex SHOULD-FIX)

In `events.rs` worker_peer_event_tests mod, add a test that deserializes a `WorkerPeerMailboxReconciled` JSON missing the new fields and the existing `audit_failed_emitted`:

```rust
#[test]
fn worker_peer_mailbox_reconciled_deserializes_with_missing_new_fields() {
    // Pre-bd-cpf.5c JSON without `inflight_already_delivered`.
    // Pre-bd-cpf.5b JSON without `audit_failed_emitted`.
    let json = r#"{
        "type": "WorkerPeerMailboxReconciled",
        "brain_session_id": "bs-1",
        "inflight_forced_to_delivered": 2,
        "inflight_stranded": 0,
        "inflight_reverted_to_queued": 0,
        "guards_re_wrapped": 1
    }"#;
    let body: SpurEventBody = serde_json::from_str(json).unwrap();
    if let SpurEventBody::WorkerPeerMailboxReconciled {
        audit_failed_emitted,
        inflight_already_delivered,
        ..
    } = body
    {
        assert_eq!(audit_failed_emitted, 0);
        assert_eq!(inflight_already_delivered, 0);
    } else {
        panic!("wrong variant");
    }
}
```

### CHANGELOG entry (under `### Added` in Unreleased)

```markdown
- **`WorkerPeerMailboxReconciled.inflight_already_delivered` counter.**
  Tracks benign idempotent races during startup reconciliation where an
  entry was already in `Delivered` state when the reconciler attempted
  to advance it. Always 0 in Stage-1; becomes non-zero under Stage-2
  crash-loop or concurrent-reconcile scenarios. (bd-cpf.5c)
```

## What this fixes

1. **Lineage projection clobber (BLOCKER)**: reconciler no longer emits `WorkerPeerMessageDelivered { injected_chars: 0 }` on race-loss. The real byte count from the orchestrator's post-prompt path is preserved.
2. **Counter accuracy**: `inflight_forced_to_delivered` no longer over-counts on idempotent races.
3. **Stage-2 visibility**: new `inflight_already_delivered` counter pre-stages observability.
4. **Replay backward compat**: `audit_failed_emitted` gets the missing `#[serde(default)]` (bd-cpf.5b oversight).

## What this preserves

- Helper API unchanged (`transition_with_audit` already returns `Unchanged(state)` distinct from `Changed`).
- `PeerTransitionKind` enum unchanged (`ReconcileToDelivered` from bd-cpf.5b is correct).
- TerminalSkip arm unchanged (bd-cpf.5b).
- AuditFailed arm unchanged (bd-cpf.5b).
- Stranded path unchanged.
- `guards_re_wrapped` arm unchanged.
- Existing reconciler tests (success path, stranded path, mixed path, audit-failed) all pass.

## Patch estimate

| File | LoC |
|---|---|
| `peer_mailbox/reconciler.rs` arm split + ReconcileCounts field | +15 |
| `spur-acp/domain/events.rs` (new field with `#[serde(default)]` + add `#[serde(default)]` to `audit_failed_emitted`) | +5 |
| New test in `reconciler.rs` (`RaceSimulatingLedger` + scenario) | +50 |
| New backward-compat test in `events.rs` | +20 |
| `CHANGELOG.md` entry | +5 |
| **Total** | **~+95 LoC** |

Risk: **low**. Helper already tested; arm split is mechanical. The only behavioral change is removing a spurious event emission and counter increment — both improvements.

## Followups (NOT bd-cpf.5c scope)

| Item | Tracking |
|---|---|
| `ReconcileCounts` derive `PartialEq + Eq` (kimi open Q2) | NIT, defer |
| Stage-2: stronger ledger contract for snapshot consistency (codex open Q3) | Stage-2 |
| Stage-2: alerting threshold for `inflight_already_delivered` (kimi finding) | Stage-2 runbook work |
| Lineage projection regression test for clobber (codex open Q2) | Could add as defensive test, but bd-cpf.5c removes the clobber source so the test would fail by construction. Skip unless reviewers want it as a tripwire. |
