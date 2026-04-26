# bd-cpf.6 — Stage-2 prep: first-principles framing

## The three sub-items

bd-cpf.6 was filed under quad-review #6 ("Stage-2 prep") covering three loosely-related items:

1. **`PeerMailboxLedger::non_terminal_entries()` filter/cursor** — claude-code flagged: "returns whole table — won't scale to SQLite at Stage-2."
2. **`Acceptance::Rejected` variant** — claude-code: name mismatch with `accept_or_reject` function.
3. **`Acceptance` `#[non_exhaustive]`** — codex defensive recommendation.

Each is a different kind of change with different urgency. They were grouped as "Stage-2 prep" but their right-now-vs-defer answers differ.

## Item 1 — `non_terminal_entries()` filter/cursor

Current API (`crates/spur-core/src/peer_mailbox/ledger.rs:130`):
```rust
async fn non_terminal_entries(&self) -> Vec<LedgerEntry>;
```

Used by the reconciler at startup to scan all non-terminal messages.

**Why claude-code flagged it**: at Stage-2 (SQLite-backed ledger), `Vec<LedgerEntry>` materializes everything in memory. If the table grows large (long-running session, many in-flight messages), this could OOM.

**Reachability today**: zero (in-memory ledger, transient non-terminal set, bounded by `max_pending_mailbox_depth=8` × number of delegations × message lifecycle).

**Stage-2 reachability**: depends on:
- Whether SQLite stores terminal-state entries persistently (if yes, are they archived/pruned?).
- Number of concurrent in-flight messages a real deployment carries.

**Pragmatic question**: do we know enough about Stage-2's actual data shape to design a cursor API today? Probably not. A speculative cursor API designed before Stage-2 lands risks being wrong-shaped (e.g., wrong cursor key, wrong filter parameters).

**Alternatives**:
- A1: Add cursor API now. `async fn non_terminal_entries_paginated(&self, cursor: Option<Cursor>, limit: usize) -> Page<LedgerEntry>`. Risk: API churn when Stage-2 lands.
- A2: Add filter parameters (e.g., `target_delegation_id: Option<&DelegationId>`). Useful for diagnostic queries but not for the core reconciler scan.
- A3: Defer entirely until Stage-2 introduces a real backing store. Document the scaling concern as a TODO comment on the trait.

## Item 2 — `Acceptance::Rejected` variant

Current shape (`router.rs:40`):
```rust
pub enum Acceptance {
    Created(PeerMessageGuard),
    AlreadyAccepted,
}

pub async fn accept_or_reject(
    &self,
    request: PeerMessageEnvelope,
    snapshot: &PlanScopeSnapshot,
) -> Result<Acceptance, RouterError> {
    // ... rejection paths return `Err(RouterError::Rejected { ... })`
}
```

**Claude-code's complaint**: function name is `accept_or_reject` but `Acceptance` has no `Rejected` variant — rejection is signaled via `Err(RouterError::Rejected)`.

**Two interpretations**:
- A: This is a real ergonomic bug. Add `Acceptance::Rejected { reason: String }` and change function to return `Acceptance` (not `Result`).
- B: This is just naming friction. The current shape (Ok = Created/AlreadyAccepted, Err = Rejected) is idiomatic Rust Result handling. Documentation can clarify.

**Counter to A**: changing to `Acceptance` (no Result) means callers can't use `?` for rejections. Most current callers in `orchestrator.rs` and `spur_ext_interp.rs` likely use `?` to propagate rejection upward as an error. Switching to a non-Result return type forces explicit match-and-bubble-up at every call site.

**Counter to B**: The mismatch is real. A code reader sees `accept_or_reject -> Acceptance` and expects rejection in the variants.

**Alternatives**:
- B1: Add `Acceptance::Rejected { reason: String }`, change function to `Result<Acceptance, RouterError>` where `RouterError` is reserved for unrecoverable errors (Ledger errors, invariant violations) — keep rejection in the Ok variant. Callers explicitly match `Acceptance::Rejected` rather than treating it as Err.
- B2: Rename function to `accept` and document that rejection is in `RouterError::Rejected`. No type changes.
- B3: Add `Acceptance::Rejected` BUT keep it in the Err position too (i.e., return `Result<AcceptResult, RouterError>` where `AcceptResult` has Created/AlreadyAccepted/Rejected). This is just renaming `Acceptance`.
- B4: Defer — name mismatch is a bikeshed; not worth the call-site churn.

## Item 3 — `Acceptance` `#[non_exhaustive]`

Current: `pub enum Acceptance { Created(PeerMessageGuard), AlreadyAccepted }` — no `#[non_exhaustive]`.

**Codex's argument**: future variants would break external exhaustive matchers. `Acceptance` is in `spur-core` re-exported via `peer_mailbox`. External callers who match exhaustively would have to add `_ => ...` if we ever add a variant.

**Cost**: ~1 line annotation. All existing matchers in the workspace need a `_ =>` arm — but they probably already have one for safety, OR they only match the variants they care about.

**Pragmatic question**: do we plan to add `Acceptance` variants in Stage-2? If yes, `#[non_exhaustive]` now is cheap insurance. If not, it's overhead for no benefit.

**Stage-2 plausible additions**:
- `Acceptance::Deferred(reason)` — message accepted but not yet routable (e.g., pending DAG resolution).
- `Acceptance::Buffered(handle)` — accepted into a buffer, not yet committed to the ledger.
- `Acceptance::Rejected(reason)` — if we adopt B1.

If any of those land in Stage-2, `#[non_exhaustive]` is right today.

**Alternatives**:
- C1: Add `#[non_exhaustive]` now. Tiny patch.
- C2: Defer until a Stage-2 variant is actually proposed.

## Scope decision

These three items have different cost/value profiles:

| Item | Patch size | Risk | Value today | Stage-2 forced? |
|---|---|---|---|---|
| 1: filter/cursor | ~30 LoC + tests | medium (API design) | low (no scaling pressure today) | possibly |
| 2: Acceptance::Rejected | varies (depends on alt) | medium (caller churn) | low (cosmetic) | no |
| 3: `#[non_exhaustive]` | ~1 LoC | low | low | yes (if any variant added) |

**Recommendation**: do **only Item 3** in bd-cpf.6, defer items 1 and 2 to separate tickets if/when they become reachable concerns.

Reasons:
- Item 3 is cheap insurance with zero downside.
- Item 1 is speculative — designing a cursor API before knowing Stage-2's data shape risks API churn.
- Item 2 is a naming bikeshed with real call-site churn for cosmetic gain.
- Three loosely-related items in one ticket dilutes review focus and makes rollback harder.

If we want to be more aggressive: also do Item 2 (B2 = rename the function to `accept`, no type changes). That's tiny and improves clarity without churning the type system.

## Asks for reviewers

1. Should bd-cpf.6 do all three items, just Item 3, or some other subset?
2. For Item 2, which alternative (B1, B2, B3, B4) fits SPUR's existing error-handling idiom?
3. For Item 1, do we have any concrete Stage-2 pressure that justifies a cursor API today, or is it speculative?
4. Does adding `#[non_exhaustive]` to `Acceptance` break any existing match site that does NOT have a `_ =>` arm?
5. Patch size and risk for your preferred scope.
