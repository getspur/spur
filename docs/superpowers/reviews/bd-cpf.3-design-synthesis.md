# bd-cpf.3 design synthesis — Alt 2b' (surgical revert-removal + Stranded event)

After multi-round first-principles analysis (`sequential-thinking` MCTS, 11 rounds), Alt 2b' is the chosen design.

## Key finding the reviewers missed

All three reviewers (claude-code, gemini, kimi) reasoned about the documented bug — *"reconciler reverts DeliveredInflight → Queued, late worker ack tries Queued → Consumed (invalid), ack swallowed"* — but that path is **unreachable in production today**.

Trace: in `orchestrator.rs:4783`, `record_injection` runs BEFORE the transition to DeliveredInflight at `orchestrator.rs:4967`. So `DeliveredInflight + injected_into_prompts.is_empty()` cannot fire from production code. The empty-injection branch in `reconciler.rs:34` only triggers when:
1. `record_injection` errors with `LedgerError::NotFound` — impossible today (no eviction)
2. Test code seeds the state directly (which existing tests do)

## Comparison matrix

| Alt | Patch | Closes bug | Audit purity | Matrix invariant | Stage-2 trajectory |
|-----|-------|-----------|--------------|------------------|---------------------|
| Today | 0 LoC | Vestigial wrong logic | Bug-on-trigger | Preserved | Reverts get more wrong with persistence |
| A: full unification | ~150 | Same as today | Same | Preserved | Refactored away by Stage-2 |
| B: shared helper | ~80 | Same as today | Same | Preserved | Refactored away |
| C: focused fix | ~30 | Yes-for-some | Stranding ambiguity | Preserved | Inherited ambiguity |
| D: B + quiet window | ~200 | Yes | Boot latency O(N) | Preserved | Refactored away |
| E1 (gemini/kimi): catalog only | ~50 | Yes | **Loses Delivered for crashed-mid-flight** | Preserved | Clean slate |
| E2 (codex): matrix relax | ~5 | Yes (theoretically) | Preserved | **Weakens Queued strictness** | Inherited weakness |
| **2b' (this)** | **~80** | **Yes** | **Preserved** | **Preserved** | **Clean Stage-2** |

## The 2b' design

1. **Keep** the non-empty-injection branch in `reconciler.rs` — when the reconciler finds a `DeliveredInflight` message with injection records, advance to `Delivered`. This is correct: we have evidence the prompt was sent; the orchestrator's post-prompt code crashed before the second transition; the reconciler completes it. Audit-pure: the `WorkerPeerMessageDelivered` event fires.
2. **Drop** the empty-injection branch's transition to `Queued` — wrong-by-design even if unreachable.
3. **Replace** with a new `SpurEventBody::WorkerPeerMessageReconciledStranded` event for observability. Operators see "reconciler found unexpected state; manual intervention required" if the unreachable case ever fires.
4. **Update** `ReconcileCounts` — add `inflight_stranded`. Keep `inflight_reverted_to_queued` for wire-compat, always 0 going forward (or remove pending consumer audit).

## What this preserves

- Matrix invariants: zero changes to `is_valid_transition`. Queued stays strict.
- Audit purity: Delivered event still fires when reconciler advances on legitimate evidence.
- Idempotency: same-state transitions remain `Ok(Unchanged)` no-ops.
- Stage-2 trajectory: Stranded event becomes a real signal once persistent ledger introduces failure modes that can reach DeliveredInflight-without-injection (eviction, async write loss, etc.).
