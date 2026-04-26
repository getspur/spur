# bd-arch.21 design synthesis — Alt H (Hybrid session-id refactor + Orchestrator::new wire-up)

After L9 sequential-thinking MCTS over the three design reviews, the chosen design is **Alt H** = boot-time wire-up in `Orchestrator::new` + `JoinHandle` in existing `background_tasks` + **codex hybrid** for the brain_session_id refactor (pass-per-emit on router, resolver on reconciler) + 3 integration tests + default-off.

## Decision matrix

| Decision | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Insertion point: `Orchestrator::new` | yes | yes | yes | **yes** | converged |
| `Limits::default()` (no config bridge) | yes | yes | yes | **yes** | converged |
| Reconciler `JoinHandle` in `background_tasks` | yes | yes (BLOCKER) | yes | **yes** | converged |
| Default `peer_mailbox_enabled = false` | yes (BLOCKER) | yes | yes | **yes** | converged |
| 3 integration tests minimum | yes | yes (+SHOULD-DOs) | yes (+more) | **3 minimum + 2 SHOULD-DO from kimi** | merged |
| Defer panic-restart supervisor | yes | yes | yes | **defer** | converged |
| Q3 brain_session_id: fix in ticket vs defer | DEFER (placeholder) | FIX (resolver) | FIX (BLOCKER) | **fix in ticket** | follow kimi+codex (2-of-3) |
| Q3 fix shape | n/a | resolver | hybrid | **codex hybrid** (per-emit on router, resolver on reconciler) | follow codex's actual recommendation |
| Three startup-reconcile sites consolidation | NICE-TO-HAVE | leave | SHOULD-DO | **defer** with code comments | follow kimi (idempotent transition is sufficient guard) |
| Match-completeness on Acceptance/RouterError | (silent) | (silent) | SHOULD-DO | **adopt** | follow codex (low cost, prevents future bugs) |

## Override rationale

### Q3 fix-or-defer: fix wins 2-of-3

Gemini argued for a placeholder (`"orchestrator-wide"`) with the refactor deferred. Kimi and codex both argued for the fix in this ticket. Codex's BLOCKER framing carries the technical weight: shipping with a known-bad design baked into 100+ emit sites is harder to undo than fixing at wire-up time.

Concretely: gemini's placeholder strategy means every `WorkerPeerMessageAccepted`, `WorkerPeerMessageRejected`, `WorkerPeerMessageDelivered`, etc. emitted by the router carries `brain_session_id = "orchestrator-wide"`. Dashboards keyed on `brain_session_id` would see all peer-mailbox traffic land in one synthetic bucket. That's not "Stage-1 acceptable staleness" — that's broken telemetry. Override gemini.

### Q3 fix shape: codex hybrid wins on technical merit

Three options:
- All pass-per-emit (codex's stated preference): cleanest type-checked correctness, but awkward for the reconciler (no caller has the session_id since `PeerMessageGuard::drop` fires outside the original call stack).
- All resolver (kimi's preference): smaller patch, but implicit global state on every emission.
- **Codex hybrid** (codex's actual recommendation in Q3 last paragraph): pass-per-emit for router methods (where callers in `WorkerAttemptCtx` already have the session_id), resolver for the reconciler (where there's no caller).

The hybrid wins because:
1. Router emissions are the hot path (every accept/reject/delivery). Type-system-enforced correctness here is highest-value.
2. Reconciler is a single emission site driven by an mpsc receiver; resolver is the natural shape.
3. This is what codex actually recommends (his "pass-per-emit is cleaner [for router], while a resolver is probably the right reconciler compromise").

### Three startup-reconcile sites: defer consolidation

Codex SHOULD-DOs consolidation. Kimi argues the existing idempotent transition logic + bd-cpf.5b/5c's "emit only on Changed" hardening means duplicate calls are safe. Add code comments noting "Idempotent; safe to call multiple times per session" at each site. Consolidation is a Stage-2 cleanup once the helper has a clearer Stage-2 contract.

## The Alt H design

### Production wire-up in `Orchestrator::new`

```rust
// crates/spur-core/src/orchestrator.rs (Orchestrator::new)

let mut orchestrator = Self { /* existing fields */, peer_mailbox: None, /* ... */ };

// existing sweep_handle pattern (line 859) precedent:
orchestrator.background_tasks.push(sweep_handle);

if config.peer_mailbox_enabled {
    let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
    let (reconciler_tx, reconciler_rx) = tokio::sync::mpsc::unbounded_channel();
    let session_slot: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        reconciler_tx,
        Limits::default(),
        // brain_session_id field REMOVED — see Q3 refactor below
    ));
    let builder = Arc::new(PeerPromptContextBuilder::new(ledger.clone()));
    let bundle = PeerMailboxBundle {
        router,
        builder,
        ledger: ledger.clone(),
        // NEW: shared slot so session-start sites can update without grabbing the bundle
        brain_session_id_slot: session_slot.clone(),
    };
    orchestrator.peer_mailbox = Some(bundle);

    let reconciler_handle = tokio::spawn(crate::peer_mailbox::run_reconciler_loop(
        reconciler_rx,
        ledger,
        funnel.clone(),
        session_slot, // resolver, not value
    ));
    orchestrator.background_tasks.push(reconciler_handle);
}
```

### Q3 refactor: pass-per-emit on router

```rust
// crates/spur-core/src/peer_mailbox/router.rs

pub struct PeerMailboxRouter {
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    limits: Limits,
    // REMOVED: brain_session_id: String,
}

impl PeerMailboxRouter {
    pub fn new(
        ledger: Arc<dyn PeerMailboxLedger>,
        funnel: FunnelHandle,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        limits: Limits,
        // brain_session_id parameter REMOVED
    ) -> Self { /* ... */ }

    pub async fn accept_or_reject(
        &self,
        brain_session_id: &str,        // NEW
        request: PeerMessageEnvelope,
        snapshot: &PlanScopeSnapshot,
    ) -> Result<Acceptance, RouterError> { /* ... uses brain_session_id at every funnel.emit ... */ }

    pub async fn record_terminal(
        &self,
        brain_session_id: &str,        // NEW
        message_id: &PeerMessageId,
        outcome: TerminalOutcome,
    ) -> Result<(), LedgerError> { /* ... */ }

    async fn reject(
        &self,
        brain_session_id: &str,        // NEW
        envelope: &PeerMessageEnvelope,
        reason: String,
    ) -> RouterError { /* ... */ }
}
```

Caller-side: every router emission site already holds the brain session id in `WorkerAttemptCtx` or equivalent. The plumbing is mechanical.

### Q3 refactor: resolver on reconciler

```rust
// crates/spur-core/src/peer_mailbox/guard.rs

pub async fn run_reconciler_loop(
    mut rx: UnboundedReceiver<StrandedMessage>,
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    session_slot: Arc<RwLock<Option<String>>>,    // CHANGED: from String
) {
    while let Some(stranded) = rx.recv().await {
        // ...
        match ledger.transition(&stranded.message_id, LedgerState::Undeliverable).await {
            Ok(TransitionOutcome::Changed { .. }) => {
                if let Some(entry) = ledger.get(&stranded.message_id).await {
                    let session_id = session_slot.read().await.clone()
                        .unwrap_or_else(|| "<no-active-session>".to_string());
                    funnel.emit(SpurEventBody::WorkerPeerMessageUndeliverable {
                        brain_session_id: session_id,  // NEW: resolved at emit time
                        message_id: stranded.message_id,
                        target_delegation_id: entry.envelope.target_delegation_id,
                        reason,
                    });
                }
            }
            // ...
        }
    }
}
```

Slot updates:
- `create_brain_session`: write the new session id to the slot.
- `load_brain_session`: write the resumed session id to the slot.
- `run_adhoc`: write the adhoc session id to the slot.
- Brain reconnect: write the new session id (or keep old if same).

The fallback `"<no-active-session>"` covers the narrow window between orchestrator boot and first session start. In practice, no peer messages should be in flight in that window (the worker spawn happens later). If the fallback string ever appears in production logs, it's a real bug worth investigating.

### `PeerMailboxBundle` extension

```rust
#[derive(Clone)]
pub struct PeerMailboxBundle {
    pub router: Arc<router::PeerMailboxRouter>,
    pub builder: Arc<prompt_builder::PeerPromptContextBuilder>,
    pub ledger: Arc<dyn ledger::PeerMailboxLedger>,
    pub brain_session_id_slot: Arc<RwLock<Option<String>>>,  // NEW
}
```

Or, if we want to avoid widening the public bundle surface, expose a `update_brain_session_id(&str)` method on the bundle that writes to the internal slot. Implementation detail — either works.

### Tests (3 SHOULD-DO + 2 NICE-TO-HAVE)

In `crates/spur-core/tests/peer_mailbox_production_wireup.rs` (new integration test file):

**SHOULD-DO (3):**
1. `peer_mailbox_enabled_true_attaches_bundle_and_spawns_reconciler` — construct Orchestrator with flag on; assert `peer_mailbox.is_some()` and a worker emitting `_spur/peer_message` reaches the ledger + emits `WorkerPeerMessageAccepted`.
2. `peer_mailbox_enabled_false_silently_drops_notification` — construct with default config; same notification; assert ledger empty + no event emitted.
3. `reconciler_drains_stranded_message` — flag on; manually create a guard and drop it without finalize; assert reconciler transitions to `Undeliverable` + emits `WorkerPeerMessageUndeliverable` with the active session id from the slot.

**SHOULD-DO from kimi (2 more):**
4. `orchestrator_drop_aborts_reconciler` — spawn orchestrator with flag on; drop it; assert reconciler `JoinHandle::is_finished()` returns true.
5. `session_slot_update_propagates_to_reconciler_emit` — flag on; update slot to "session-A", drop a guard, assert event has `brain_session_id == "session-A"`; update slot to "session-B", drop another guard, assert event has `brain_session_id == "session-B"`.

### Match-completeness discipline (codex SHOULD-DO)

In production code paths (router callers in orchestrator + spur_ext_interp), explicitly handle every `Acceptance` and `RouterError` variant. No `_ => {}` arms in spur-core. Same-crate exhaustive matching catches future variants at compile time.

### CHANGELOG entry

Under `## Unreleased` → `### Added`:

```markdown
- **Peer mailbox production wire-up (Stage-1).** The peer mailbox subsystem
  (hardened in bd-cpf.1–7) is now constructed and attached when
  `peer_mailbox_enabled = true` is set in config. A long-lived reconciler
  task drains stranded peer messages and emits audit events. Startup
  reconcile runs at brain session boundaries. Default is `false`; no
  behavioral change for existing deployments. Operators who opt in should
  monitor for `WorkerPeerMessageUndeliverable` events and be aware that
  the in-memory ledger does not prune entries (Risk #22). To disable,
  set `peer_mailbox_enabled = false` and restart SPUR — runtime toggle
  is not supported. (bd-arch.21)
```

Under `### Fixed`:

```markdown
- **Architecture Risk #21.** The peer mailbox reconciler is now spawned
  at orchestrator boot and aborted on shutdown via `Orchestrator::drop`.
  Previously the receiver was dropped immediately after construction,
  causing stranded messages to be silently lost — but the surrounding
  wire-up was also missing in production, so the entire subsystem (62
  tests, bd-cpf.1–7 hardening) was inert. (bd-arch.21)
```

## What this fixes

1. **Architecture Risk #21**: production peer-mailbox subsystem is wired, reconciler is spawned, JoinHandle is tracked.
2. **Architecture Risk #6 (partial)**: the reconciler `tokio::spawn` is tracked via `background_tasks`, so it doesn't add to the un-tracked spawn count.
3. **Latent staleness bug** in router/reconciler `brain_session_id` capture: refactored to per-emit (router) + resolver (reconciler) so events always carry the active session id.

## What this preserves

- Default-off behavior: zero new code paths execute for existing deployments.
- bd-cpf.1–7 hardening: all 62 existing peer_mailbox tests + drain-lifecycle events continue to pass without modification.
- `PeerMailboxBundle` shape: clone-friendly, owns the ledger via Arc.
- Existing `background_tasks` shutdown ordering.
- Three startup-reconcile call sites (defer consolidation; idempotent transitions + bd-cpf.5b/5c "emit only on Changed" hardening prevents duplicates).

## What this does NOT do

- Flip `peer_mailbox_enabled = true` (separate follow-up after internal validation).
- Add a `Limits` config surface (separate ticket if operator tuning is needed).
- Add panic-restart supervision (deferred; reconciler body is small and infallible).
- Address Risk #22 ledger pruning (separate ticket per user directive).
- Consolidate the three `run_startup_reconcile` call sites (Stage-2 cleanup).

## Patch estimate

| Area | LoC |
|---|---|
| `spur-core/src/orchestrator.rs` (Orchestrator::new wire-up + slot threading) | +50 |
| `spur-core/src/peer_mailbox/router.rs` (remove field, add per-method param) | +30 -10 |
| `spur-core/src/peer_mailbox/guard.rs` (resolver in run_reconciler_loop) | +15 -5 |
| `spur-core/src/peer_mailbox/mod.rs` (`PeerMailboxBundle` extension) | +5 |
| Caller sites (orchestrator emit sites + spur_ext_interp) | +20 -5 |
| Session-slot update sites (`create_brain_session`, `load_brain_session`, `run_adhoc`) | +15 |
| Tests (5: 3 SHOULD-DO + 2 from kimi) | +120 |
| `CHANGELOG.md` | +20 |
| **Total** | **~+275 / -20 ≈ +255 net** |

Risk: **medium**. The default-off path is strictly additive (zero behavior change). The default-on path exercises bd-cpf.1–7 hardening with zero production miles, but is gated behind explicit opt-in. The brain_session_id refactor touches every router emit site but is mechanical (caller already has the id in scope).

## Followups (NOT bd-arch.21 scope)

| Item | Tracking |
|---|---|
| `peer_mailbox_enabled = true` default flip | New ticket after internal validation |
| `Limits` config surface | New ticket if operator tuning becomes a real need |
| Architecture Risk #22 (ledger pruning) | bd-arch.22 (already pending per user directive) |
| Three `run_startup_reconcile` call sites consolidation | Stage-2 cleanup |
| Panic-restart supervisor for reconciler | Stage-2 (when persistent ledger makes durability matter) |
| `Orchestrator::drop` graceful drain (vs abort) for reconciler | Stage-2 (when SQLite ledger commits matter pre-shutdown) |
| Architecture Risk #6 full fix (TaskTracker for all spur-core spawns) | Separate Risk #6 ticket |
