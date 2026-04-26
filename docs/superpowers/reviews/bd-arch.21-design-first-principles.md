# bd-arch.21 — peer mailbox production wiring + reconciler spawn: first-principles framing

## What the architecture doc claims

From `docs/architecture.md` Risk #21 (severity High):

> Peer mailbox reconciler never spawned — `run_reconciler_loop` is defined but the receiver is dropped immediately after construction (`let (_, _rx) = unbounded_channel()`). Stranded messages are silently lost; ledger entries leak in `Accepted`/`Queued` forever.

## What the territory actually shows

Direct code grounding (this branch, after bd-cpf.1–7):

1. **`attach_peer_mailbox()` is dead**. Defined at `crates/spur-core/src/orchestrator.rs:884`. Zero call sites in production code, only test fixtures. `Orchestrator.peer_mailbox: Option<PeerMailboxBundle>` is therefore always `None` in production.

2. **`peer_mailbox_enabled` is read by nothing**. Config field at `crates/spur-acp/src/config/mod.rs:375`. Tests assert the default is `false`. No production code reads the flag — there is no `if config.peer_mailbox_enabled { construct + attach }` block anywhere.

3. **`run_reconciler_loop` is defined but never spawned**. `crates/spur-core/src/peer_mailbox/guard.rs:101`. The doc-comment says "Spawned at orchestrator boot; survives across attempts" — but no boot code spawns it.

4. **`run_startup_reconcile` IS wired** at three sites (orchestrator.rs:1150, 2540, 2806), each guarded by `if let Some(bundle) = self.peer_mailbox.clone()`. Because `peer_mailbox` is always `None`, all three call sites are dead.

5. **`_spur/peer_message` ext-notification handlers in `spur_ext_interp.rs`** also gate on `bundle.is_some()`. With no production attach, peer-message notifications from workers are silently dropped at the `_spur/*` boundary.

So the gap is bigger than the architecture doc framing: **the entire peer mailbox subsystem (62 tests + bd-cpf.1–7 hardening + 7,887 lines of orchestrator integration) is currently inert in production.** The reconciler-not-spawned issue is one symptom of a missing boot-time wire-up.

## Stage-1 vs Stage-2 context

Per `docs/superpowers/plans/2026-04-25-worker-peer-mailbox-stage1.md`, the original plan was:
- **Stage-1** (this work, now hardened by bd-cpf.1–7): in-memory ledger, single-process, opt-in via `peer_mailbox_enabled`.
- **Stage-2** (future): persistent ledger (SQLite), cross-restart durability, multi-drain reachability.

bd-arch.21 is the production wire-up for Stage-1. Without it, the bd-cpf series is preventive hardening for code that never runs.

## The four sub-problems bd-arch.21 must solve

### Problem A — Boot-time bundle construction + attach

Where: somewhere between config load and orchestrator `run_interactive`/`run_adhoc` start.

```rust
if config.peer_mailbox_enabled {
    let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
    let (reconciler_tx, reconciler_rx) = tokio::sync::mpsc::unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        reconciler_tx,
        Limits::default(), // or from config
        brain_session_id.to_string(),
    ));
    let builder = Arc::new(PeerPromptContextBuilder::new(ledger.clone()));
    let bundle = PeerMailboxBundle { router, builder, ledger };
    orchestrator.attach_peer_mailbox(bundle);
    // also: keep reconciler_rx for problem B
}
```

Open questions:
- Where in the boot sequence? `spur-cli` constructs `Orchestrator`; the cleanest insertion point is right after construction, before any `run_*` call.
- Do we need a `Limits` config surface, or is `Limits::default()` adequate for Stage-1 production? bd-cpf.4 added `drain_max_total_ms` with default 10_000. If operators want to tune, we need a config bridge.
- `brain_session_id` is per-session, not per-orchestrator. The current `PeerMailboxRouter::new` takes a `brain_session_id: String` parameter — but a single orchestrator handles many brain sessions over its lifetime. Is the router per-session or per-orchestrator? Reading the code: bundle is attached once via `attach_peer_mailbox`; emit sites pass `brain_session_id` per-event. So the field on the router is used only for events the router itself emits. This means we'd need to either (a) refactor the router to not own a `brain_session_id`, or (b) put the bundle inside `OrchestratorState` and rebuild it per-session. (b) breaks the "spawned at orchestrator boot; survives across attempts" doc-comment on `run_reconciler_loop`.

### Problem B — Spawn `run_reconciler_loop`

Once `reconciler_rx` is owned by the boot code, spawn a long-lived task:

```rust
let reconciler_handle = tokio::spawn(crate::peer_mailbox::run_reconciler_loop(
    reconciler_rx,
    ledger.clone(),
    funnel.clone(),
    brain_session_id.to_string(),
));
```

Open questions:
- **Lifetime**: where does this task live until? Until orchestrator shutdown. Currently the orchestrator does not have a single shutdown handle list — the existing fire-and-forget pattern (`tokio::spawn` without storing JoinHandle) is itself listed as Risk #6. Storing the JoinHandle is necessary if we want clean shutdown.
- **Panic restart**: per architecture-doc remaining-work item 1, "spawn `run_reconciler_loop` and add a panic-restart supervisor." Is panic restart in bd-arch.21 scope, or a follow-up? The reconciler loop body is mostly `match` against `ledger.transition` and event emission — the panic surface is small. A `JoinHandle::is_finished()` check on shutdown might be sufficient initial coverage, with restart deferred.
- **`brain_session_id` lifetime**: `run_reconciler_loop` takes `brain_session_id: String` and emits `WorkerPeerMessageUndeliverable` with that session id baked in. If the brain reconnects with a new session id mid-orchestrator-lifetime, the reconciler still emits with the OLD session id. Need to either: (a) make the reconciler take a `Fn() -> String` resolver, (b) restart the reconciler on every session swap, or (c) accept the staleness for Stage-1.

### Problem C — Boot ordering for `run_startup_reconcile`

`run_startup_reconcile` is currently called from inside `run_interactive`/`run_adhoc` at three sites. With production wire-up, these sites finally fire when `peer_mailbox.is_some()`.

Open questions:
- The startup reconcile is per-brain-session today (called inside the brain spawn path). Is that correct? Or should it be a single per-orchestrator boot pass? In Stage-1 with in-memory ledger, every restart gives a fresh ledger, so the startup pass is moot at process-boot time but useful at brain-session-restart time (where the orchestrator survives but a new brain takes over).
- The three call sites are slightly different: `:1150` is in one path, `:2540` and `:2806` in others. Are they all reachable, and do they all need the production wire-up to fire?

### Problem D — Config-flag default

`peer_mailbox_enabled: bool` defaults to `false`. Should bd-arch.21 flip it to `true`? Reasoning:
- If false-by-default ships: the wire-up exists but no one uses it. Operators must opt in. Conservative.
- If true-by-default ships: every SPUR install starts evaluating peer messages. New event volume, new code paths exercised. Possible regression risk.

bd-cpf.1–7 hardened the implementation, but it has zero production miles. Conservative default is probably right. A follow-up flip-the-default ticket can be filed once SPUR-internal users have validated.

## Correctness/safety constraints

1. **Compile-time existence**: removing the `Option<PeerMailboxBundle>` and making it required would tighten types but break the "off by default" deployment story. Keep `Option<...>` for now; verify all gate sites still gate.
2. **Shutdown ordering**: if `run_reconciler_loop` is spawned, on orchestrator shutdown the reconciler must drain remaining StrandedMessages OR be aborted cleanly. Either way, the existing peer-message notifications in flight should not panic.
3. **Test impact**: 62 existing peer_mailbox tests construct fixtures inline. None of them go through `attach_peer_mailbox` — they own bundles directly. So the new wire-up adds a fresh integration test surface but does not break existing tests.
4. **Architecture doc Risk #6**: this commit increases the count of `tokio::spawn` calls by 1 — but if we store the JoinHandle (which we should), it does NOT add to the un-tracked count. Storing it actually helps Risk #6.

## Reachability today vs after fix

| Path | Today | After bd-arch.21 (with default-off) | After flag-flip |
|---|---|---|---|
| Worker emits `_spur/peer_message` | dropped silently at `_spur/*` boundary | dropped silently (config off) | accepted by `PeerMailboxRouter` |
| Worker drops `PeerMessageGuard` mid-flight | impossible (guard never created) | impossible (config off) | reconciler forces Undeliverable + emits audit event |
| Brain session restart | no peer mailbox at all | no peer mailbox (config off) | startup reconcile catches stranded entries |
| Long-running session | no leak (no entries) | no leak (no entries) | leak per Risk #22 (ledger has no pruning) |

## Design questions for reviewers

1. **Insertion point**: where should the boot-time bundle construction live? `spur-cli` (closest to config load), or inside `Orchestrator::new`/`run_interactive` (encapsulated)?

2. **`Limits` config surface**: should bd-arch.21 expose `Limits` fields (drain_quiet_window_ms, drain_max_total_ms, max_pending_mailbox_depth) in `SpurConfig`, or hardcode `Limits::default()` for Stage-1?

3. **`brain_session_id` lifetime on the bundle**: is the router's `brain_session_id` field a code smell? Should it be removed and passed per-emit-call, OR should the bundle live inside per-session state and be rebuilt on session swap?

4. **JoinHandle tracking**: should bd-arch.21 add an explicit shutdown handle (e.g., a `Vec<JoinHandle<()>>` on Orchestrator) and abort the reconciler on shutdown? Or accept fire-and-forget for Stage-1 and defer to a future Risk #6 work?

5. **Panic-restart supervisor**: in scope or deferred? The reconciler loop body is small and mostly infallible. A simple `JoinHandle::is_finished()` warning on shutdown might be enough for Stage-1.

6. **Config-flag default**: keep `peer_mailbox_enabled: false`-by-default in this ticket, with a follow-up to flip it once we have production miles? Or flip it now to maximize bd-cpf.1–7's value?

7. **Tests**: minimum coverage = (a) integration test that with `peer_mailbox_enabled=true` and a worker emitting `_spur/peer_message`, the message reaches the ledger and an event is emitted; (b) integration test that with `peer_mailbox_enabled=false` (default), the same notification is silently dropped (no router invocation, no event emission); (c) test that the reconciler task drains a manufactured StrandedMessage. Anything else essential?

8. **Three startup-reconcile call sites**: now that the peer mailbox might actually be `Some`, we need to confirm all three are reachable and necessary, OR consolidate them into a single helper. This matters because in bd-cpf.5b/5c we hardened the reconciler's audit emission — duplicate calls would emit duplicate `WorkerPeerMailboxReconciled` events.

9. **Stage-2 forward compatibility**: does the wire-up shape we choose (where bundle is constructed, where reconciler is spawned, etc.) generalize to Stage-2 (persistent ledger), or will Stage-2 require re-architecting the boot sequence?

## Out of scope

- **Risk #22 (unbounded ledger pruning)**: deferred to a separate ticket. Reviewer may comment on whether bd-arch.21's choice of insertion point makes Risk #22 easier or harder, but pruning logic does NOT belong in this ticket.
- **Flipping `peer_mailbox_enabled` default to true**: follow-up after this ticket validates in real workloads.
- **Panic-restart supervisor**: per-question 5; either in scope (if reviewers think it's cheap) or deferred.
- **Risk #6 (general TaskTracker introduction in spur-core)**: only the reconciler JoinHandle is in scope.

## Cost estimate

- Boot-time bundle construction + attach: ~30 LoC
- `run_reconciler_loop` spawn + JoinHandle storage: ~20 LoC
- Limits config surface (if endorsed): ~40 LoC
- Tests (integration + reconciler-task): ~80 LoC
- Total: **~120–170 LoC**

Risk: **medium**. Strictly additive in production — if `peer_mailbox_enabled=false` (default), behavior is unchanged. The risk is `peer_mailbox_enabled=true` paths exercising bd-cpf.1–7 code with zero production miles. The risk is mitigated by (a) keeping default off, (b) the integration test in question 7 (a) demonstrating the end-to-end happy path, and (c) the existing 62 peer_mailbox tests covering the underlying logic.
