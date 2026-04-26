# bd-cpf.6 Operational Review — Kimi

**Commit under review:** `32b1737` — `feat(spur-core): bd-cpf.6 mark Acceptance #[non_exhaustive] for Stage-2 forward-compat`
**Reviewer:** kimi  
**Date:** 2026-04-26  
**Scope:** Item 3 only (`#[non_exhaustive]` on `Acceptance` + test wildcard arms). Items 1 (cursor API) and 2 (rename `accept_or_reject` → `accept`) explicitly out of scope per L9 MCTS synthesis.

---

## Verdict

**LGTM-with-NITs**

---

## Pager-risk classification

**Low** — purely compile-time change; zero runtime impact.

---

## Issues

### SHOULD-FIX

#### 1. Doc-comment loss — operational invariant explanation removed
- **File:** `crates/spur-core/src/peer_mailbox/router.rs`  
- **Lines:** 30–40 (doc comment on `Acceptance`)  
- **Problem:** The original doc comment explained the critical spec invariant *"at most one guard exists per message at any time"* and the causal chain from replay → second guard → drop → stranded enqueue → reconciler marks in-flight original as `Undeliverable`. The commit replaces this with the forward-compat note, so a future on-call engineer debugging a stranded-message incident loses the only in-code explanation of why `AlreadyAccepted` exists and what goes wrong if it is absent.
- **Minimal-edit suggestion:** Merge both explanations. Replace the current doc comment with:

```rust
/// Outcome of a successful router accept attempt. Distinct from rejection
/// (which is signaled via `RouterError::Rejected`) and ledger errors
/// (which surface via `RouterError::Ledger`).
///
/// Distinguishes a fresh acceptance (caller receives a guard and is
/// responsible for finalize) from a replay (caller receives nothing —
/// the original handler still owns the guard). This separation is
/// critical for spec invariant "at most one guard exists per message at
/// any time": if we returned a fresh guard on replay, dropping it would
/// enqueue a stranded message and the reconciler would forcibly mark the
/// in-flight original as Undeliverable. The `AlreadyAccepted` variant
/// prevents that.
///
/// Forward-compat note: marked `#[non_exhaustive]` so future variants
/// (e.g., `Deferred`, `Buffered` for Stage-2 persistent ledger) can be
/// added without breaking external matchers. Internal same-crate matches
/// remain exhaustive — the compile-time pressure to handle new variants
/// is preserved where it matters most.
```

---

### NIT

#### 2. CHANGELOG tracking entry
- **File:** `CHANGELOG.md`  
- **Lines:** `## Unreleased` → `### Added`  
- **Problem:** bd-cpf.5b/5c added `Unreleased` entries. bd-cpf.6 is invisible to operators, but a one-line tracking note helps future migration docs and dashboards know when `#[non_exhaustive]` was applied.
- **Minimal-edit suggestion:** Add under `### Added`:

```markdown
- **Peer mailbox `Acceptance` enum is now `#[non_exhaustive]`** for Stage-2
  forward-compat. No runtime impact; external matchers must add wildcard
  arms. (bd-cpf.6)
```

*Acceptable alternative:* silence is defensible if the project convention is to only CHANGELOG observable behavior changes.

---

## Direct answers to operational questions

### 1. Pager risk: does this change any runtime behavior?

**No — zero runtime impact.**

`#[non_exhaustive]` is a compile-time attribute that affects exhaustiveness checking only for downstream crates (integration tests and any future external consumers). It emits no extra instructions, changes no enum layout, and introduces no branching. The test-side wildcard arms are unreachable with the current two-variant enum; they would only fire if a future variant is added, at which point the panic is the desired failure mode.

### 2. CHANGELOG: should bd-cpf.6 get an Unreleased entry?

**A one-line tracking note is recommended but not required.**

There is no user-visible or operator-visible behavior change, so silence is consistent with a "observable changes only" CHANGELOG policy. However, `#[non_exhaustive]` is an API contract change that affects downstream matchers. Adding a brief `Added` bullet makes it searchable for migration notes and dashboard auditors who want to know when the attribute was introduced. If the convention is strict about observable impact, silence is correct.

### 3. Doc-comment loss: is the removed invariant explanation a SHOULD-FIX?

**Yes — SHOULD-FIX.**

The original comment was the *only* place in the codebase that connected `Acceptance::AlreadyAccepted` to the stranded-message / reconciler / `Undeliverable` causal chain. When an on-call engineer investigates a stranded-message incident at 3 AM, they will grep for `Acceptance` and `AlreadyAccepted`. The new forward-compat note tells them *what* the enum is, but not *why the variant exists* or *what breaks if it disappears*. Merging both explanations (see Issue 1 above) costs ~6 lines and preserves operational context.

### 4. External consumers: any non-test crates exhaustively matching `Acceptance`?

**None.**

Verified with `git grep -n "Acceptance::" -- '*.rs'` filtered to exclude `crates/spur-core/src/` and `crates/spur-core/tests/` — no matches. All same-crate production matches (`router.rs`, `orchestrator.rs`, `spur_ext_interp.rs`) remain exhaustive and are unaffected by `#[non_exhaustive]`. The change is safe for the current workspace.

### 5. Stage-2 readiness: does `#[non_exhaustive]` still buy useful freedom?

**Yes — the calculus holds.**

The L9 synthesis identified plausible Stage-2 variants (`Deferred`, `Buffered`, potentially `Rejected`). With `#[non_exhaustive]`:

- **External consumers** (integration tests today, future workspace crates tomorrow) will compile without breakage when new variants are added, because their wildcard arms already cover the new cases.
- **Production same-crate code** (`spur-core/src/`) retains exhaustive match pressure — adding a variant will force developers to handle it in `orchestrator.rs`, `spur_ext_interp.rs`, and `router.rs`, which is exactly where handling matters most.

The ~16 LoC insurance premium is minimal compared to the future churn it avoids.

---

## Summary

The implementation exactly matches the synthesized design: `#[non_exhaustive]` on `Acceptance`, wildcard arms in the two external integration test files, no changes to same-crate production matches. The only operational concern is the loss of the invariant explanation in the doc comment, which should be restored before merge. Once that is addressed, this is ready to land.
