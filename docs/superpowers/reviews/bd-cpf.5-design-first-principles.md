# bd-cpf.5 — post-prompt symmetric helper extraction: first-principles framing

## The duplication

`orchestrator.rs:4968-5059` runs immediately after `prompt_result` succeeds and the worker has been prompted. For each peer-message injection record, two ledger transitions are attempted in sequence:

1. `Accepted → DeliveredInflight` (block 1, lines 4969-5009)
2. `DeliveredInflight → Delivered` (block 2, lines 5011-5058)

Each block is structurally a 4-arm match on the `LedgerError` shape, with identical bodies for 3 of 4 arms.

### Side-by-side structure

```
match bundle.ledger.transition(&inj.message_id, <STATE>).await {
    Ok(Changed { .. })    => { /* per-state side effect */ },
    Ok(Unchanged(state))  => debug!("... transition no-op"),
    Err(InvalidTransition { from, .. }) if is_terminal(from)
                          => { debug!("... skipped: already terminal"); continue; }
    Err(err)              => { warn!(...); emit AuditFailed { transition_kind: "..." }; }
}
```

### The 10% that differs

| Arm | DeliveredInflight block | Delivered block |
|---|---|---|
| `Ok(Changed)` | empty `{}` | emit `WorkerPeerMessageDelivered { ..., target_prompt_id, injected_chars }` + TODO comment |
| `Ok(Unchanged)` | identical (only log msg differs in word: "delivered-inflight" vs "delivered") | identical |
| `Err(terminal)` | identical (only log msg word differs) | identical |
| `Err(other)` | emits `transition_kind: "delivered_inflight"` | emits `transition_kind: "delivered"` |

Quad-review consensus: **90% duplication, extract helper**.

## Why now (not later)

- bd-cpf.3 (just-merged) added a similar transition in the reconciler with the same audit-failure shape — there's already a third site that could use the same helper.
- bd-cpf.4 left no behavioral changes here, so the structural cleanup is safe pre-Stage-2.
- Stage-2 (persistent ledger) will introduce new failure modes (e.g., `LedgerError::Eviction`) that should be handled centrally. Refactoring now means Stage-2 only touches the helper, not 2-3 call sites.

## Design questions

1. **Helper signature shape**:
   - `async fn transition_post_prompt(ledger, funnel, message_id, target_state, brain_session_id, target_delegation_id, on_changed: F) -> Result<(), ()>` where `on_changed: impl FnOnce()` for the unique side effect?
   - `async fn transition_with_audit(ledger, funnel, message_id, target_state, transition_kind: &str, brain_session_id, target_delegation_id) -> TransitionResult` returning a typed result for the caller to act on?
   - Inline closure passed in vs returned-result polled by caller?

2. **Where does the helper live?**
   - Free function in `orchestrator.rs`?
   - Method on `PeerMailboxBundle`?
   - Module under `crates/spur-core/src/peer_mailbox/`?

3. **Should the reconciler also use it?**
   - bd-cpf.3 reconciler.rs:53-82 has a near-identical structure for `DeliveredInflight → Delivered`.
   - Reconciler is post-startup, not post-prompt — different `transition_kind` string + different post-Changed event (`WorkerPeerMessageDelivered { injected_chars: 0 }` per bd-cpf.3 distortion comment).
   - Pulling reconciler into the helper would unify 3 sites; leaving it would be 2 sites.

4. **Async closure ergonomics**:
   - Pre-Rust-1.85 async closures need workarounds (return `Pin<Box<dyn Future>>` or keep the on-changed action sync).
   - All current `Ok(Changed)` side effects are sync (`funnel.emit`) so a sync `FnOnce` closure should suffice.

5. **Error propagation**:
   - Today: each block continues to the next iteration on terminal-skip, falls through to the next block on other errors. The fall-through behavior matters: a failure on `DeliveredInflight` doesn't prevent the `Delivered` attempt, but the second is guaranteed to fail with `InvalidTransition` from `Accepted`.
   - Helper should preserve this: caller decides whether to early-return or chain.

## Alternatives

### Alt A — Free function with sync closure for `on_changed`

```rust
async fn record_post_prompt_transition<F>(
    bundle: &PeerMailboxBundle,
    funnel: &FunnelHandle,
    brain_session_id: &BrainSessionId,
    target_delegation_id: &DelegationId,
    message_id: PeerMessageId,
    target_state: LedgerState,
    transition_kind: &'static str,
    on_changed: F,
) -> ControlFlow<(), ()>
where
    F: FnOnce(),
{ ... }
```
Caller iterates injection records and calls helper twice (DeliveredInflight then Delivered), passing different `on_changed` closures and reacting to `ControlFlow::Break` (terminal-skip should `continue` outer loop).

**Pro**: Smallest refactor. Helper has 4 lines of match per call.
**Con**: Two call sites in the same loop iteration, plus `ControlFlow` plumbing.

### Alt B — Free function returning typed result, caller chains

```rust
enum PostPromptOutcome {
    Transitioned,
    NoOp,
    AlreadyTerminal,
    AuditFailed,
}

async fn try_post_prompt_transition(
    bundle, funnel, ..., target_state, transition_kind,
) -> PostPromptOutcome { ... }
```
Caller handles outcomes outside the helper; `Ok(Changed)` side effects stay at the caller.

**Pro**: No closure trait bounds. Side effects fully visible at the call site.
**Con**: Caller still has 4-arm match on `PostPromptOutcome`. Mostly moves the duplication elsewhere.

### Alt C — Two specialized helpers

```rust
async fn record_delivered_inflight(bundle, funnel, ctx, message_id) { ... }
async fn record_delivered(bundle, funnel, ctx, message_id, target_prompt_id, injected_chars) { ... }
```
One helper per transition target. Each has its own audit string and side effect baked in.

**Pro**: Each helper is unambiguous. Type-checks the side-effect parameters.
**Con**: Doubles the helper count. The shared 90% lives in a deeper layer (e.g., `transition_with_audit_log`), so we'd actually want 3 layers.

### Alt D — Method on `PeerMailboxBundle`

`bundle.record_transition(funnel, ctx, message_id, target_state, on_changed)`. Same signature as Alt A but as a method on the existing bundle type.

**Pro**: Discoverable via the bundle type. `funnel` could come from the bundle (fewer args).
**Con**: `funnel` isn't currently stored on the bundle — it's owned by the orchestrator. Adding a funnel ref to the bundle is a bigger change than the refactor warrants.

### Alt E — Layered helper: `transition_with_audit` + per-call wrappers

Bottom layer: `async fn transition_with_audit(ledger, funnel, brain_session_id, target_delegation_id, message_id, target_state, transition_kind) -> TransitionAuditOutcome`.
Top layer: callers (post-prompt, reconciler) wrap with their site-specific `Ok(Changed)` side-effect.

**Pro**: Stage-2 can extend `transition_with_audit` once; reconciler also uses it (3 sites unified).
**Con**: Two layers means slightly more code than Alt A or B. But it's the most extensible shape.

## First-principles questions

1. **What's the right scope: 2 sites or 3?** Pulling the reconciler in is bigger but has higher payoff. The reconciler's `DeliveredInflight → Delivered` arm in `peer_mailbox/reconciler.rs:53-82` already has the same audit shape (debug, warn, AuditFailed). Unifying 3 sites pre-Stage-2 is high-value.

2. **Is `ControlFlow` overkill for the terminal-skip case?** The current code uses `continue` to skip to the next injection record. A helper that returns `bool` ("should caller continue outer loop?") is simpler than `ControlFlow`. Or the helper could not return at all and let the caller fall through (the second transition will fail naturally if the first hit a terminal).

3. **Should the helper own emitting the post-Changed event?** If yes, it needs the side-effect data (target_prompt_id, injected_chars, etc.) as parameters — leaks the Delivered specifics into a generic helper. If no, the caller emits, helper just transitions.

4. **What about the `tracing::debug!` log message wording differences?** They're cosmetic. A helper can use a `transition_kind` string to interpolate the log message. Trivial.

5. **Stage-2 forward-compat:** Stage-2 will likely add `LedgerError::Eviction`, `LedgerError::Persistence`, etc. Does the helper need to handle those today, or just centralize so adding them later is one-place?

## My provisional recommendation: Alt E (layered)

`transition_with_audit` as the bottom layer, returning a typed outcome the caller maps to `Ok(Changed) | NoOp | TerminalSkip | AuditFailed`. Caller iterates, maps outcomes to its site-specific side effects (post-prompt emits Delivered; reconciler emits Delivered with `injected_chars: 0`; future call sites can map differently).

Three sites unified (post-prompt × 2 transitions + reconciler × 1 transition). Stage-2 adds new error variants in one place.

Patch size estimate: ~80 LoC for the helper + ~50 LoC dedup at call sites = ~130 LoC net (but ~-60 deletions).

## Asks for reviewers

1. Pick A/B/C/D/E (or your own F). Why?
2. Should the reconciler be folded in (3 sites) or left out (2 sites) for Stage-1?
3. Closure-vs-typed-return: which is more idiomatic for Rust async/await?
4. Where does the helper live (orchestrator.rs free fn, peer_mailbox module, bundle method)?
5. Patch size and risk for your preferred alternative.
