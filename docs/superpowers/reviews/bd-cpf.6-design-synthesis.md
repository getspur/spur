# bd-cpf.6 design synthesis — Item 3 only (`#[non_exhaustive]` on `Acceptance`)

After 2-round L9 sequential-thinking MCTS over the three design reviews, the chosen scope is **only Item 3**: add `#[non_exhaustive]` to the `Acceptance` enum, plus the ~10 test-side wildcard arms required for compilation.

## Decision matrix

| Item | Gemini | Kimi | Codex | **Synthesis** | Override |
|---|---|---|---|---|---|
| Item 1 (cursor API) | defer (BLOCKER) | defer | defer | **defer** | converged |
| Item 2 (rename `accept_or_reject` → `accept`) | do (B2) | defer (cosmetic) | defer (acceptable but not now) | **defer** | follow kimi+codex |
| Item 3 (`#[non_exhaustive]`) | do | do | **defer** (softened from original) | **do** | follow gemini+kimi (2-of-3) |
| Item 2 B1/B3 (Rejected variant) | reject | reject | reject | **reject** | converged |

## Override rationale

1. **gemini's "do B2 rename" → defer**: kimi (cosmetic, zero pager value) and codex (acceptable but defer) outweigh gemini's single endorsement. The function name `accept_or_reject` is operationally clear in logs and traces (where `RouterError::Rejected { reason }` is what shows up). Renaming creates churn at every call site for marginal code-reading aesthetics. Defer to a separate ticket if the friction ever becomes real.

2. **codex's softened "defer Item 3" → do Item 3**: 2-of-3 endorse. Codex's softening rests on "exhaustive matches in tests are useful pressure during API evolution" — but `#[non_exhaustive]` only affects DOWNSTREAM crates (integration `tests/` are external). Production same-crate matches in `crates/spur-core/src/` keep exhaustive matching. Tests usually match-and-panic on unexpected variants anyway, so the loss of pressure there is negligible. Insurance value is real (Stage-2 may add `Acceptance::Deferred`/`Buffered`/`Rejected`) for ~15 LoC cost.

## The Item-3-only design

### Production change

```rust
// crates/spur-core/src/peer_mailbox/router.rs:38-43
/// Outcome of a successful router accept attempt. Distinct from rejection
/// (which is signaled via `RouterError::Rejected`) and ledger errors
/// (which surface via `RouterError::Ledger`).
///
/// Forward-compat note: marked `#[non_exhaustive]` so future variants
/// (e.g., `Deferred`, `Buffered` for Stage-2 persistent ledger) can be
/// added without breaking external matchers. Internal same-crate matches
/// remain exhaustive — the compile-time pressure to handle new variants
/// is preserved where it matters most.
#[derive(Debug)]
#[non_exhaustive]
pub enum Acceptance {
    Created(PeerMessageGuard),
    AlreadyAccepted,
}
```

### Test wildcard arms (~10 sites)

Per codex's grep, the affected sites are:
- `crates/spur-core/tests/peer_mailbox_e2e.rs:122, 184, 243, 315, 433`
- `crates/spur-core/tests/peer_mailbox_concurrency.rs:217, 456, 467, 493, 564`

Each existing `match` block needs a wildcard arm:

```rust
match acceptance {
    Acceptance::Created(guard) => { /* existing */ }
    Acceptance::AlreadyAccepted => { /* existing */ }
    _ => panic!("unexpected Acceptance variant"),
}
```

For replay-specific assertions (e.g., `concurrency.rs:467`), use a more specific message:
```rust
_ => panic!("replay returned an unexpected Acceptance variant"),
```

### Same-crate matches (UNAFFECTED — verified by codex)

These remain exhaustive without changes:
- `crates/spur-core/src/peer_mailbox/router.rs:303`
- `crates/spur-core/src/orchestrator.rs:6087`
- `crates/spur-core/src/spur_ext_interp.rs:500, 843, 955`

Plus `matches!(acceptance, Acceptance::Created(_))` at `tests/peer_mailbox_concurrency.rs:171` (macro expands to wildcard already).

### Tests

No new tests required — the wildcard arms are inert (will only fire if someone adds an unhandled variant in the future, which is exactly the desired forward-compat property).

Existing peer_mailbox tests (currently 62) must still pass.

## What this preserves

- All current behavior: rejection still returns `Err(RouterError::Rejected)`; same `Result<Acceptance, RouterError>` signature; same caller `?` propagation.
- Production same-crate exhaustive matching pressure (`spur_ext_interp.rs`, `orchestrator.rs`, `router.rs` itself).
- Codex's preferred design: rejection stays in Err, no new variant churn, no cursor speculation.

## What this enables

- Future Stage-2 additions to `Acceptance` (Deferred, Buffered, etc.) won't break external matchers.
- A future external `spur-core` consumer (e.g., a separate workspace or external binding) can match on `Acceptance` with a `_ =>` arm without compile breakage when variants are added.

## Patch estimate

| File | LoC |
|---|---|
| `peer_mailbox/router.rs` (`#[non_exhaustive]` + doc comment) | +6 |
| `tests/peer_mailbox_e2e.rs` (5 wildcard arms) | +5 |
| `tests/peer_mailbox_concurrency.rs` (5 wildcard arms) | +5 |
| **Total** | **~+16 LoC** |

Risk: **very low**. The annotation is purely additive; wildcard arms are inert until a new variant is added.

## Followups (NOT bd-cpf.6 scope)

| Item | Tracking |
|---|---|
| `non_terminal_entries()` cursor API (Item 1) | **Stage-2** — wait for persistent ledger schema/index/retention design |
| Rename `accept_or_reject` → `accept` (Item 2 B2) | Defer — naming friction not yet operationally real |
| Rename `Acceptance` to `AcceptResult` (Item 2 B3) | Reject — semantics already idiomatic |
| Add `Acceptance::Rejected` variant (Item 2 B1) | Reject — would force rejection-as-Ok pattern; not idiomatic for SPUR's existing `?` propagation |
| Persistent ledger read-error model | **Stage-2** — should be designed alongside cursor API, not standalone |
| Stage-2 plausible variants (`Deferred`, `Buffered`) | Wait for Stage-2 design; `#[non_exhaustive]` from this PR makes the addition non-breaking |
