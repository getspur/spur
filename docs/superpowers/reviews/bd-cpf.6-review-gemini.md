# Correctness Review: bd-cpf.6

**Commit reviewed:** `32b1737`  
**Verdict:** Block on SHOULD-FIX

## Issues

*   **SHOULD-FIX** - `crates/spur-core/src/peer_mailbox/router.rs:27-39`: Doc comment regression.
    The original comment explained a critical spec invariant ("at most one guard exists per message at any time") and why `AlreadyAccepted` prevents stranded messages. This vital domain context was replaced rather than appended to.
    **Fix:** Merge both paragraphs. Restore the invariant documentation alongside the new forward-compat note.

*   **NIT** - `crates/spur-core/tests/peer_mailbox_concurrency.rs` & `crates/spur-core/tests/peer_mailbox_e2e.rs`:
    The wildcard arms use `_ => panic!("unexpected Acceptance variant")`. Since `Acceptance` derives `Debug`, it might be slightly more helpful to bind and format the value (e.g., `other => panic!("unexpected Acceptance variant: {other:?}")`) to immediately surface *which* unexpected variant broke the test, though failing line numbers are usually sufficient.

## Correctness Verification Answers

1.  **Does the diff stay strictly within Item-3 scope?**
    **Yes.** The diff solely focuses on `#[non_exhaustive]`, the associated documentation changes, and the integration test wildcard fallback arms. Items 1 and 2 (cursor API and `accept_or_reject` -> `accept` rename) were successfully excluded.

2.  **Production same-crate matches unchanged and exhaustive?**
    **Verified.** The commit explicitly left `crates/spur-core/src/peer_mailbox/router.rs:303`, `crates/spur-core/src/orchestrator.rs:6087`, and `crates/spur-core/src/spur_ext_interp.rs:500/843/955` unchanged. Since they are within the same crate, the compiler will continue to enforce exhaustive matching on them when new variants are added.

3.  **Doc-comment regression: SHOULD-FIX?**
    **Yes.** This is raised as a SHOULD-FIX above. The lost context regarding the reconciler and stranded messages is critical domain knowledge that must be retained alongside the forward-compat note.

4.  **Are wildcard arms placed safely?**
    **Yes.** The wildcard arms intentionally fail loudly using `panic!(...)`. If future variants (like `Deferred` or `Buffered`) unexpectedly reach the code paths tested in `concurrency.rs:468` or `495`, the test will panic instead of silently ignoring them, ensuring any assumption breaks are immediately caught.

5.  **Does `matches!` still compile cleanly?**
    **Confirmed.** The `matches!(acceptance, Acceptance::Created(_))` macro expands into a `match` expression with an implicit `_ => false` fallback arm. This inherent wildcard behavior natively supports `#[non_exhaustive]` enums without issue.

6.  **Any `#[derive]` interactions to worry about?**
    **No.** `#[derive(Debug)]` generates exhaustive matching code *inside* the defining crate (`spur-core`). Because `#[non_exhaustive]` only forces fallback arms for usages strictly *outside* the defining crate, `Debug` and other built-in derives work perfectly.
