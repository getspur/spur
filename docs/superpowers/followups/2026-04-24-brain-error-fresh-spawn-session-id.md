# Follow-up: BrainError at orchestrator.rs:1737 uses `SessionId::new()` on fresh-spawn failure

**Discovered during:** Tranche 1 implementation of session-resume fix (2026-04-24).
**Related commits:** `272cb5f`, `d03e119` (fixed the same defect on the resume path at lines 1358 and 1431).
**Scope:** ~4 LOC, plus one regression test if a harness is reasonable.

## Defect

`crates/spur-core/src/orchestrator.rs:1737-1740` emits:

```rust
self.emit(SpurEvent::now(SpurEventBody::BrainError {
    session: SessionId::new(),
    message: error_message,
}));
```

This fires when brain spawn fails on the **non-resume** flow (fresh prompt, no brain attached yet). `SessionId::new()` generates a fresh random UUID rather than carrying the session identity the caller was operating on. Same defect pattern as Tranche 1's resume-path fixes (commits `272cb5f`, `d03e119`), but in a different code path so out of scope for that tranche.

## Why this matters (FP-5: event correlation requires faithful identity)

Downstream consumers of `BrainError` (spur-bot, session_detail, dashboard, app) cannot correlate the error to the session the user was interacting with. Under the optimistic-navigation redesign in Tranche 2, this will prevent `SessionDetailView` from transitioning to its `Failed` state on fresh-spawn errors.

## Fix shape

At line 1737, thread whatever session identity is in scope at that point into the event. If no session id has been established yet (pre-spawn), consider a well-known sentinel (`SessionId::from("<pre-spawn>")`) or `Option<SessionId>` via an event-schema variant change. The simplest fix: find the nearest in-scope SessionId local and clone it; if there is none, adding a sentinel is preferable to `SessionId::new()`.

## Acceptance

- `BrainError` emitted at the fresh-spawn failure path carries a stable/traceable session id (not a per-event fresh UUID).
- If feasible, add a regression test modeled on `crates/spur-core/tests/brain_error_session_correlation.rs`. If infeasible without a mock ACP harness, document the gap in the same way Task 3 did.

## Priority

Low. Tranche 2 (optimistic navigation UX) does not strictly require this fix to land, but the `SessionDetail.Failed` state transition is more reliable with it.
