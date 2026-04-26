# bd-arch.21 design review - Codex

## Recommendation

Proceed with bd-arch.21 as a narrowly scoped Stage-1 production wire-up, but do
not wire the current `PeerMailboxRouter::new(..., brain_session_id: String)` or
`run_reconciler_loop(..., brain_session_id: String)` signatures into production
as-is. The Rust-idiom shape is:

1. Move peer-mailbox construction into `spur-core`, preferably inside
   `Orchestrator::new`, guarded by `config.peer_mailbox_enabled`.
2. Keep `PeerMailboxBundle` owned by `Orchestrator` for orchestrator-lifetime
   state, not per-brain-session state.
3. Remove the fixed brain session id from the router and reconciler. Pass the
   active session id at each emission boundary, or store a small shared
   session-id resolver/slot on the orchestrator-owned peer-mailbox runtime.
4. Track the long-lived reconciler handle in the existing
   `Orchestrator.background_tasks: Vec<JoinHandle<()>>`; do not introduce a
   full task tracker in this ticket.
5. Keep `peer_mailbox_enabled` false by default, use `Limits::default()` for
   Stage-1, and add integration coverage for enabled/disabled boot behavior.

Classification:

- BLOCKER: Do not ship with a fixed router/reconciler brain session id captured
  at bundle construction.
- BLOCKER: Spawned reconciler must keep its `UnboundedReceiver`; constructing
  the channel and dropping `_rx` is still the architecture bug.
- SHOULD-DO: Construct and attach inside `Orchestrator::new`, not every CLI
  entry point.
- SHOULD-DO: Push the reconciler handle into `background_tasks`, because that
  field already exists and `Drop` aborts it.
- SHOULD-DO: Add explicit production-consumer handling for every current
  `Acceptance` and `RouterError` variant.
- NICE-TO-HAVE: Expose limits in config after Stage-1 has production miles.
- NICE-TO-HAVE: Add panic restart supervision later unless implementation adds
  meaningful new panic surfaces.

Patch-size estimate: 150-230 LoC if the session-id fix is pass-per-emit, 230-320
LoC if bd-arch.21 introduces a shared active-session resolver. A full task
tracker, config limit surface, or persistent ledger bridge should remain out of
scope.

## Q1 - insertion point

The clean insertion point is `Orchestrator::new`, immediately after the funnel
exists and before returning the constructed orchestrator. The code already has
the ingredients that should define a peer-mailbox runtime boundary:
`config.peer_mailbox_enabled`, `self.funnel`, `background_tasks`, and the
orchestrator-owned `peer_mailbox: Option<PeerMailboxBundle>`.

Putting construction in `spur-cli` is less idiomatic here because the CLI has
multiple orchestration entry points (`load_orchestrator`, TUI construction,
interactive host construction, init/agent helpers). Duplicating a bundle helper
around those call sites would make "enabled" a CLI concern even though the
mailbox is consumed by `spur-core` worker attempts and startup reconciliation.
The existing `attach_peer_mailbox` method is fine for tests, but production
should not rely on external callers remembering to pair config parsing with a
second mutation.

Lifetime and borrow analysis:

- `reconciler_rx` is single-owner state and cannot live in the cloneable
  `PeerMailboxBundle`.
- The boot code that creates `(reconciler_tx, reconciler_rx)` must spawn the
  long-lived task before `Orchestrator::new` returns, or must store a dedicated
  runtime owner that is moved into the orchestrator.
- `PeerMessageGuard` only needs the cloneable `reconciler_tx`, so the router can
  hold the sender inside the bundle while the receiver is owned by the spawned
  task.
- `Orchestrator::new` can move `reconciler_rx` into `tokio::spawn(...)`, then
  push the returned `JoinHandle<()>` into `background_tasks`.
- This avoids borrowing `self` across a spawned task. The task receives cloned
  `Arc<dyn PeerMailboxLedger>`, cloned `FunnelHandle`, and the receiver by
  value.

If implementation wants to keep construction after `let mut orchestrator = Self
{ ... }`, add a private `enable_peer_mailbox(&mut self)` helper that creates the
ledger, router, bundle, spawns the receiver, and pushes the handle. That keeps
the move semantics localized and leaves `attach_peer_mailbox` as a test-only or
manual override helper.

## Q2 - limits config surface

Use `Limits::default()` for bd-arch.21. The feature remains opt-in, and the
current risk is production reachability, not operator tuning. Adding config
surface now would widen this ticket across `spur-acp` config parsing,
validation, docs, and tests without proving a user need.

The important Stage-1 discipline is to make the default explicit at construction
and leave a TODO or follow-up issue for:

- `drain_quiet_window_ms`
- `drain_max_total_ms`
- `max_pending_mailbox_depth`
- `max_peer_message_size`

Do not hide the limits behind ad hoc constants in orchestrator code. Construct
with `Limits::default()` so the future config bridge has one obvious replacement
site.

Classification: NICE-TO-HAVE for bd-arch.21, not a blocker.

## Q3 - brain_session_id lifetime on PeerMailboxRouter

The router's stored `brain_session_id: String` is a code smell for production
wire-up. The bundle is orchestrator-lifetime state, while `brain_session_id` is
brain-session-lifetime state. Capturing a session id at bundle construction
would make all router-originated events stale after a warm resume or new brain
session inside the same orchestrator.

I would not rebuild the bundle per session. That fights the design comment on
the reconciler loop ("spawned at orchestrator boot; survives across attempts")
and will become worse in Stage-2, where the ledger is durable process/runtime
state rather than a disposable session object. It also makes the receiver
lifetime awkward: a per-session bundle would imply either killing the
reconciler on session change or sharing the receiver across bundle generations,
both of which are more error-prone than separating runtime state from session
identity.

Preferred Rust-idiom refactor for Stage-1:

- Remove `brain_session_id` from `PeerMailboxRouter`.
- Add a `brain_session_id: &BrainSessionId` or `&str` parameter to methods that
  emit events:
  - `accept_or_reject(...)`
  - `record_terminal(...)`
  - private `reject(...)`
- Keep methods that only inspect limits or ledger state session-id free.
- Update ext-notification consumers to pass the session id they already have in
  `WorkerAttemptCtx`.

This is the most explicit and easiest to review: event emission requires a
session id in the call signature, so stale identity cannot be hidden in a
long-lived object.

Second-best option:

- Add an `Arc<ActiveBrainSession>` slot, implemented as a small
  `Arc<tokio::sync::watch::Sender/Receiver<Option<String>>>` or
  `Arc<std::sync::RwLock<Option<String>>>`.
- Router and reconciler resolve the current session id at emission time.
- `run_interactive`, `create_brain_session`, and `load_brain_session` update the
  slot when the active brain changes.

This helps if too many call sites would otherwise thread the id through, but it
is more implicit and needs policy for "no active session" at emission time. For
Stage-1, pass-per-emit is cleaner.

Do not choose "store brain_session_id on PeerMailboxBundle" unless the bundle is
renamed and treated as per-session state. That choice is less compatible with
Stage-2 persistence and with an orchestrator-lifetime reconciler.

Classification: BLOCKER.

## Q4 - JoinHandle tracking

`Orchestrator` already has `background_tasks: Vec<JoinHandle<()>>`, and its
`Drop` implementation aborts every stored handle. bd-arch.21 should use this
field for the peer-mailbox reconciler handle.

Recommended shape:

- In the enable helper, spawn the reconciler loop.
- Push the returned `JoinHandle<()>` into `background_tasks`.
- Keep the handle unstructured for now; do not add a general `TaskTracker`.

This is the cheapest idiomatic tracking shape because it matches existing
ownership and shutdown behavior. It also avoids worsening the existing
fire-and-forget spawn risk. If a future Risk #6 task introduces a named task
registry, it can migrate this handle along with the sweep task.

Classification: SHOULD-DO.

## Q5 - panic-restart supervisor

Do not implement a polling supervisor with `JoinHandle::is_finished()` in the
main orchestrator loop. That pattern is easy to forget to poll and does not
compose well with shutdown. A wrapper task that re-spawns the worker task is
idiomatic when restart is required, but it adds a second loop, policy for
backoff, and policy for closed receivers.

For bd-arch.21, I recommend accepting a single reconciler task and logging at
shutdown if the join handle is already finished only if that can be added
without changing the generic `background_tasks` shape. The current loop body is
small: receive a stranded message, attempt a ledger transition, optionally read
the entry, emit an event, log errors. There is no obvious panic boundary in
normal operation.

If restart is required in this ticket, use a dedicated supervisor task rather
than external polling:

- supervisor owns the receiver;
- inner processing is factored into `process_stranded_message(...) -> Future`;
- supervisor catches task failure at the per-message boundary if using spawned
  workers, or uses explicit error returns instead of panics;
- on channel close, supervisor exits.

But that is not a free patch. I would defer restart supervision and require a
test that dropping a guard reaches the spawned reconciler while enabled.

Classification: NICE-TO-HAVE for restart, SHOULD-DO for avoiding
fire-and-forget.

## Q6 - config flag default

Keep `peer_mailbox_enabled` false by default. bd-arch.21 should make the flag
real, not flip rollout policy. The subsystem has broad integration with worker
notifications, ledger state, injected prompt context, drain timeouts, and
lineage events; opt-in production miles are the right next step.

The implementation should still be easy to enable. A false default is not an
excuse for dead code: tests must construct config with
`peer_mailbox_enabled = true` and prove the production boot path attaches the
bundle and spawns the reconciler.

Classification: SHOULD-DO.

## Q7 - tests

Minimum coverage should be:

1. `peer_mailbox_enabled = false`: `Orchestrator::new` leaves
   `peer_mailbox == None` and does not spawn the peer reconciler.
2. `peer_mailbox_enabled = true`: `Orchestrator::new` attaches a bundle whose
   router, builder, and ledger share the same ledger instance.
3. Enabled path: dropping a `PeerMessageGuard` from a created acceptance is
   observed by the spawned reconciler and moves the entry to
   `Undeliverable`.
4. Production consumer path: a `_spur/peer_message` ext-notification reaches
   `interpret_peer_message`, creates a ledger entry, and returns
   `Acceptance::Created`.
5. Replay path: the same peer message returns `Acceptance::AlreadyAccepted`
   without creating a second guard or duplicate accepted event.
6. Disabled path: the same ext-notification is ignored at the peer-mailbox
   boundary and does not mutate a ledger.

For startup reconciliation, add either one integration-ish orchestrator test or
one focused core test that a manufactured non-terminal entry is reconciled once
and that duplicate startup calls do not double-emit the delivered/reconciled
event. This matters because the three startup-reconcile call sites become live
after bd-arch.21.

Classification: SHOULD-DO.

## Q8 - startup-reconcile call sites

The three existing `run_startup_reconcile` call sites are currently dead only
because the bundle is never attached. Once enabled, they should be reviewed as
"per brain session start" hooks, not process boot hooks.

That is defensible for Stage-1. The in-memory ledger makes process-start
reconciliation mostly moot; the useful case is orchestrator-survives,
brain-session-changes, and there are non-terminal entries from previous worker
attempts. However, the call sites should be consolidated into a helper such as
`self.run_peer_mailbox_startup_reconcile(&brain_session_id).await`. That reduces
the chance that one path receives a future fix and the others diverge.

Duplicate calls are mostly protected by idempotent transition handling, but
bd-cpf.5b/5c made audit emission correctness important. The helper should make
the policy obvious:

- run once per newly active brain session;
- use the active brain session id at call time;
- trace `ReconcileCounts`;
- do not emit duplicate lifecycle events on unchanged states.

Classification: SHOULD-DO to consolidate or at least add coverage proving no
duplicate emission across reachable paths.

## Q9 - Stage-2 forward compatibility

The recommended shape generalizes to Stage-2. An orchestrator-owned bundle with
`Arc<dyn PeerMailboxLedger>` is already the right abstraction for replacing
`InMemoryLedger` with a persistent backend. The boot helper can choose
`InMemoryLedger` for Stage-1 and a SQLite-backed ledger for Stage-2 without
changing worker attempt code, prompt building, or ext-notification parsing.

What should change before Stage-2:

- Do not bake session identity into the ledger, router, or long-lived
  reconciler. Session identity is event metadata, not mailbox storage identity.
- Keep `run_reconciler_loop` independent of a fixed session id. Either pass a
  resolver for event emission or move event emission to a function that receives
  the active session id.
- Make ledger construction fallible. Stage-1 `InMemoryLedger::new()` cannot
  fail; Stage-2 SQLite open/migration can. The boot helper should be prepared to
  return `Result<()>` and fail orchestrator construction if
  `peer_mailbox_enabled = true` but the persistent ledger cannot open.
- Consider startup reconcile as a persistent-ledger recovery pass. In Stage-2,
  process boot matters because entries survive restart; the helper should be
  callable both when the orchestrator starts and when a brain session becomes
  active.

`run_reconciler_loop`'s receiver-based shape can stay. The signature should
change only for session identity:

Current:

```rust
run_reconciler_loop(rx, ledger, funnel, brain_session_id: String)
```

Stage-1 production recommendation:

```rust
run_reconciler_loop(rx, ledger, funnel, session_resolver)
```

or, if all stranded recovery can be associated with message metadata in the
ledger later:

```rust
run_reconciler_loop(rx, ledger, funnel)
```

The latter is attractive for Stage-2 if `PeerMessageEnvelope` or ledger entries
carry enough session/event routing metadata. Until then, pass-per-emit is the
least surprising Stage-1 router refactor, while a resolver is probably the
right reconciler compromise because guard drops happen outside the original
accept call stack.

Classification: BLOCKER for removing fixed session-id capture; NICE-TO-HAVE for
persistent-ledger boot shape in this ticket.

## Match completeness notes

`Acceptance` is `#[non_exhaustive]`, but same-crate matches remain exhaustive.
Production consumers added by bd-arch.21 should match the current variants
explicitly:

- `Acceptance::Created(guard)`: retain the guard until the message is either
  injected and finalized or deliberately terminalized.
- `Acceptance::AlreadyAccepted`: treat as idempotent replay, do not create or
  drop a new guard, and do not emit duplicate accepted events.

Do not use `_ => {}` inside `spur-core` for `Acceptance`. The internal compile
pressure is valuable when Stage-2 adds variants such as deferred or buffered
acceptance.

`RouterError` consumers should also be explicit:

- `RouterError::Rejected { reason }`: request-level rejection; already emits a
  rejected event from router validation when rejection happens inside the
  router. Parse-layer rejections from `interpret_peer_message` may need a
  malformed/rejected boundary event if operators must see them.
- `RouterError::Ledger(err)`: storage/state-machine failure; warn and avoid
  retrying blindly unless the ledger error is known transient in Stage-2.
- `RouterError::InvariantViolation(msg)`: internal bug signal; warn/error with
  enough context to diagnose message id and delegation ids.

The new wire-up should avoid a single `if let Ok(Acceptance::Created(_)) = ...`
shape because that silently discards replays and typed failures, exactly the
kind of invisibility bd-arch.21 is meant to remove.

## Final call

bd-arch.21 should wire the subsystem at orchestrator boot, keep default-off
rollout, and use existing `background_tasks` ownership. The only design issue I
would block on is session-id lifetime: a long-lived mailbox runtime must not
store a single brain session id. Fix that now with explicit per-emission session
ids for the router and a resolver or session slot for the reconciler; the rest
can land as a focused Stage-1 production enablement patch.
