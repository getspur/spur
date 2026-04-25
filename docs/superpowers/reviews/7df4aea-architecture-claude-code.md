# Reviewing merge commit 7df4aea — architecture/forward-compat angle

Scope: D (1190747 ack-wiring, c1b8de7 matrix relax, 35548a7 TOCTOU), E (a41fea3, ec11d15 drain race), F (53bd294, 72cf2dc proptest invariants), G (78ec61e, c60dccf concurrency tests). Production deltas: `peer_mailbox/ledger.rs`, `peer_mailbox/router.rs` (unchanged on this chain — surface only), `peer_mailbox/reconciler.rs`, `orchestrator.rs`, `spur_ext_interp.rs`. Tests-only files skipped per remit.

## API-surface forward-compat

- **DESIGN-NOTE** `is_terminal` and `is_valid_transition` are `#[doc(hidden)] pub` for proptest access (`peer_mailbox/ledger.rs:56-110`). `#[doc(hidden)]` is the right escape hatch but it's still a semver-affecting symbol; Stage-2 should move both behind an explicit `#[cfg(any(test, feature = "ledger-internals"))]` boundary or relocate the proptests under `crates/spur-core/src/peer_mailbox/ledger.rs` `#[cfg(test)] mod tests` so the matrix predicates stay genuinely private. Right now any external crate can call them, and the function-signature-as-spec guarantees we accidentally promised will become Stage-2 baggage.
- **SHOULD-FIX** `Acceptance` (`peer_mailbox/router.rs:31-34`) has only `Created` / `AlreadyAccepted` — there is no `Rejected` variant despite the `accept_or_reject` name. Rejections come back as `Err(RouterError::Rejected{..})`, which conflates *I refuse this envelope* with *the ledger blew up* (both end up as `RouterError::Ledger(String)` losers of structure at `router.rs:95,160`). Stage-2 will want to discriminate persistent-backend errors from semantic rejection without string-matching.
- **DESIGN-NOTE** `RouterError::Ledger(String)` (`router.rs:18`) flattens `LedgerError::{NotFound, InvalidTransition, AlreadyTerminal}` into a string at the boundary. Stage-2 callers that want to retry on transient SQLite errors but not on `InvalidTransition` will need the underlying enum preserved — change to `RouterError::Ledger(#[from] LedgerError)` now while there are <10 call sites.

## Trait surface vs persistent backend

- **SHOULD-FIX** `PeerMailboxLedger` (`peer_mailbox/ledger.rs:112-131`) leaks two `InMemoryLedger` assumptions: (a) `non_terminal_entries() -> Vec<LedgerEntry>` returns the whole table — fine for HashMap, catastrophic for SQLite at scale. Add a `target_delegation_id` filter parameter or a paginating cursor before Stage-2. (b) `pending_for_target` returns `Vec<LedgerEntry>` cloned wholesale; Stage-2 will want streaming or at minimum `&[LedgerEntry]` via async iterator. Both are public-trait changes — easier to land alongside ack wiring than after.
- **DESIGN-NOTE** No `delete`/`evict`/`prune` method, and `router.rs:174-184` explicitly comments "InMemoryLedger never removes entries" as load-bearing. A persistent backend with retention will need an eviction surface that serializes against transitions; design the trait method now with that constraint named.

## Matrix stability for Stage-2

- **DESIGN-NOTE** The relaxed matrix (`ledger.rs:78-110`) covers `Accepted → Consumed|Ignored` and `DeliveredInflight → {Consumed,Ignored,Expired,Dropped}` — chosen for worker-ack timing, not for plan-replay. Stage-2 plan-replay scenarios where an in-flight worker is restarted without ever reaching `DeliveredInflight` will need `Queued → Consumed|Ignored` (worker re-acked from a snapshot prompt). Worth documenting *why Queued is intentionally strict* directly in the function comment so the next person doesn't relax it speculatively.

## Drain semantics

- **SHOULD-FIX** `drain_peer_acks_with_timeout` (`orchestrator.rs:5197-5252`) and `run_startup_reconcile` (`peer_mailbox/reconciler.rs:16`) are convergent design — both walk non-terminal entries for a delegation and force-terminal on quiet. The reconciler explicitly TODOs `drain_quiet_window` as unused (`reconciler.rs:22-27`). Unify into one function pre-Stage-2, parameterized by `(scope: SingleDelegation | AllNonTerminal, behavior: ForceImmediate | WaitQuietWindow)`. Two divergent code paths will rot.

## Observability

- **NIT** Add NOW (cheap, additive): `WorkerPeerMessageDrainTimedOut { delegation_id, forced_count }` so the `drain_timeout` reason at `orchestrator.rs:5240` is queryable without grepping `WorkerPeerMessageIgnored.reason`. Also `WorkerPeerMessageAckReceived { message_id, source: "worker"|"drain" }` so the funnel records the ack edge directly instead of inferring it from the lifecycle event.
- **DESIGN-NOTE** Add LATER (forces v2 schema bump): `WorkerPeerMessageStateTransition { from, to, source }` — generic edge event would replace half the discrete `Worker*` variants but is a breaking move; defer to v2.

## Production code smells

- **SHOULD-FIX** `orchestrator.rs:4906-5010` — symmetric `DeliveredInflight` and `Delivered` arms are 90% duplicated copy-paste with only the transition target and `transition_kind` string differing. Extract `apply_post_prompt_transition(state, kind_label, …)` so future state additions don't require keeping two arms in sync.
- **SHOULD-FIX** `spur_ext_interp.rs:146` — `let _ = ack_tx.send(())` discards send failure unconditionally. Drain receiver dropped early is the *expected* path so explicit drop is fine, but it should be commented why the error swallow is intentional, not a typo. Also: ack is sent even on `record_terminal` failure (line 138-145 errors logged but flow continues). That asymmetry — failed terminal still acks the drain window — is load-bearing-but-undocumented; Stage-2 must decide whether failed-terminal should reset or short-circuit the quiet window.
- **NIT** `Acceptance::Rejected` missing — see API-surface above.

## TODOs left in chain

1. **Load-bearing**: `reconciler.rs:22-27` (drain_quiet_window unused) — startup-reconcile force-terminals immediately, defeating in-flight grace on orchestrator restart. Bug-shaped.
2. **Load-bearing-latent**: `prompt_builder.rs:77` reads `injected_into_prompts` without writing — `record_injection` happens later in the orchestrator. Two concurrent `build_for_target` calls for the same target can both pass the check (the merge note flags this as Stage-2). Document inline that the check is *advisory* until Stage-2 closes the race.
3. **Cosmetic**: `orchestrator.rs:4806` (`context_window_chars` hardcoded 200_000), `orchestrator.rs:4969` (Task 14 cross-ref), `orchestrator.rs:3610` (arg consolidation). All safe to defer.

## Stage-2 readiness assessment

Stage-1 hardening achieves its stated goal: ack wiring closes the drain semantics gap, the matrix relaxation accommodates real worker-ack timing without losing Queued strictness, and the TOCTOU fix is the right shape (atomic transition error pattern, not pre-check). The proptest + concurrency suites are solid harness for Stage-2 to inherit. Blockers for Stage-2 are not in this chain — they're in the trait surface (`non_terminal_entries` cardinality, `RouterError::Ledger(String)` type erasure) and in the unfixed reconciler/drain duplication. None of those need to land before merging this chain, but they should land before the persistent-backend PR opens.

**Verdict: APPROVE-WITH-FIXES.** Approve as-is for Stage-1 scope; file follow-ups for: (1) `RouterError::Ledger` typed wrapping, (2) `PeerMailboxLedger::non_terminal_entries` filter param, (3) reconciler/drain unification, (4) extract symmetric post-prompt transition arms.
