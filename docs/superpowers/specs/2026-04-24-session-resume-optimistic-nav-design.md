# Session Resume — Optimistic Navigation & Correlated Events

**Status:** Approved — ready for implementation plan
**Date:** 2026-04-24
**Scope:** `crates/spur-core/src/orchestrator.rs`, `crates/spur-tui/src/views/session_picker.rs`, `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/app.rs`, `crates/spur-acp/src/domain/events.rs`

## Problem

When a user selects a session from the session picker, the UI halts: a spinner appears and never clears, the session never opens. Root-cause investigation identified four distinct invariant violations:

- **I-1 Event faithfulness.** `orchestrator.rs:1358` and `:1424` emit `BrainError { session: SessionId::new(), ... }` — a freshly generated random id rather than the session the user tried to resume. The event carries garbage identity.
- **I-2 Derived state, not cached.** `session_picker.rs:990` sets `resuming: bool = true` as a local cache of orchestrator state. Nothing in the codebase clears it on failure.
- **I-3 Bounded exits.** `orchestrator.rs:222` and `:251` contain unbounded `guard.await` calls inside `shutdown_mcp_server`. A stuck MCP guard hangs the entire resume pipeline with no timeout.
- **I-4 Event reachability.** `app.rs:1249-1258` dispatches `SpurEvent` to four views but not `session_picker`. Even if the picker tried to react to `BrainError`, events never reach it.

Any one of these defects independently reproduces the symptom. They must be fixed together.

## Non-goals

- Warm-agent daemon / zero-cold-start resume.
- Rewriting the ACP native transport's empty history-stream behavior (`native.rs:975`).
- Replacing the TUI event dispatch architecture.

## First principles (the design is derived from these)

- **FP-1.** A view's responsibility is one concept. The picker picks; it does not orchestrate async operations.
- **FP-2.** Async state belongs to the view that renders the async result.
- **FP-3.** Every state must have a bounded exit — either a timeout or a guaranteed typed event.
- **FP-4.** Events are the single source of truth. View-local caches of event-derived state are defects by construction.
- **FP-5.** Event correlation requires faithful identity.
- **FP-6.** Perceived latency on navigation is minimized by navigating first and hydrating second.
- **FP-7.** Warm and cold paths have order-of-magnitude-different latency classes. No single wall-clock timeout fits both.

## Approach

**Optimistic navigation with server-side bounded retirement and event-driven SessionDetail state.**

When the user presses Enter on a session row, the picker dispatches `ResumeSession` and navigates to `SessionDetail` in the same tick. The picker carries no pending state. `SessionDetail` renders a `LoadState` derived from the most recent milestone event for the target session id. The orchestrator's teardown phase is bounded at every await, and all error events carry a faithful session id so `SessionDetail` can correlate.

### Architecture

Two tranches; tranche 2 depends on tranche 1.

**Tranche 1 — server correctness (~20 LOC)**

1. `orchestrator.rs:1358` and `:1424`: replace `session: SessionId::new()` with the `session_id` already in scope on the resume path. Matches the pattern already used at `:1492`.
2. `orchestrator.rs:222` and `:251`: wrap both `guard.await` calls in `tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, ...)`. On elapse: log a structured warning keyed on `session`, drop the guard (`AbortOnDropHandle` aborts the task on drop), and return. Does not emit a new event variant; the existing `McpShutdownTimeout` event at `:241-244` already covers the outer timeout.

**Tranche 2 — UX redesign (~130 LOC)**

3. `session_picker.rs`: remove the `resuming: bool` field from `PickerState::Populated` and all its render/input sites. On Enter: dispatch `Action::ResumeSession { session_id }` and `Action::NavigateTo(ViewId::SessionDetail { session_id })` in the same tick.

4. `session_detail.rs`: introduce a `LoadState` enum as a pure projection of milestone events received for this view's `session_id`:
   ```rust
   enum LoadState {
       Retiring,                // default initial state when navigated to from picker
       Connecting { brain_name: String },
       Loading,
       Ready,
       Failed { message: String },
   }
   ```
   `LoadState` is computed each render from the last-seen milestone event matching the current `session_id`. No timers, no cached booleans. A `Failed` state renders the error message plus a "back to picker" action.

5. `app.rs:1249-1258`: add `session_picker` to the `handle_spur_event` dispatch list so the picker receives list-refresh and cancellation events. Its `handle_spur_event` implementation stays minimal; this does not reintroduce pending-state caching.

6. New milestone events in `events.rs` (additive, `Serialize`/`Deserialize`, following the existing `BrainReconnecting/BrainReconnected/BrainReconnectFailed` precedent at `events.rs:494-515`):
   - `SessionRetireStart { from: Option<SessionId>, to: SessionId }`
   - `SessionRetireComplete { session: SessionId }`
   - `BrainConnecting { session: SessionId, brain_name: String }`
   - `SessionLoading { session: SessionId }`
   - `SessionLoaded { session: SessionId }`

   Emitted at the matching phase boundaries in `orchestrator.rs`'s resume pipeline. All existing `SpurEventBody` consumers use `_ =>` catch-all arms, so these variants roll out without touching any other consumer.

### Data flow (resume from picker)

```
user Enter on row for session S
  ├─ picker: dispatch ResumeSession(S) + NavigateTo(SessionDetail(S))  [same tick]
  └─ orchestrator:
       SessionRetireStart{from, to=S}
       retire_active_brain (bounded by MCP_SHUTDOWN_TIMEOUT at every await)
       SessionRetireComplete{S}
       BrainConnecting{S, brain_name}                [only if cold]
       BrainSpawned{S}                                [existing event]
       SessionLoading{S}
       load_brain_session
       SessionLoaded{S}   OR   BrainError{session=S, message}
```

`SessionDetail` derives its `LoadState` from the newest milestone for `S`. Any `BrainError` whose `session == S` transitions `SessionDetail` to `Failed`. Events for other sessions are ignored.

### Error handling

- Orchestrator-side: existing error paths (`BrainError`, `BrainConnectFailed`, `McpShutdownTimeout`) continue to fire. Tranche 1 ensures `session` fields are correct.
- UI-side: `SessionDetail` in `Failed` state shows the error message and offers "back to picker." The picker is re-entered in its stable listing state; no spinner could be stuck because the picker holds no pending state.

### Testing

Each test guards a first principle.

- **FP-5 guard** (`spur-core`): emit `BrainError` on the resume-failure path; assert `event.body.session == requested_session_id`. Regression against re-introducing `SessionId::new()`.
- **FP-3 guard** (`spur-core`): `shutdown_mcp_server` completes within `MCP_SHUTDOWN_TIMEOUT + ε` when the guard task is stuck. Inject a guard that never completes; assert total elapsed under the timeout plus a small margin and `McpShutdownTimeout` is emitted.
- **FP-1 / FP-6 guard** (`spur-tui`): picker Enter navigates to `SessionDetail` in one tick; picker state contains no pending/resuming field.
- **FP-2 / FP-4 guard** (`spur-tui`): `SessionDetail` initial render for an unloaded session derives its label from the latest milestone event; receiving `BrainError` with matching `session` transitions to `Failed`; non-matching `session` is ignored.
- **Integration** (`spur-tui`): drive a fake orchestrator through the full picker → SessionDetail → loaded path; assert no pending flag exists anywhere.
- **Serde round-trip** (`spur-acp`): fixture containing both old `BrainError` and new milestone variants round-trips through serde unchanged. Proves forward-compat.

## Blast radius

Grounded against the codebase:

- All 5 `SpurEventBody` consumer files use catch-all `_ =>` arms → new milestone variants roll in without touching consumers.
- No test asserts `session == SessionId::new()` → faithful `BrainError` is a pure upgrade.
- `resuming` is referenced only inside `session_picker.rs` → delete is fully local.
- `AbortOnDropHandle` drop semantics match the bounded-await design (drop aborts the task) → no resource leak from timeout path.
- Event log serde: backward-compat preserved (old logs have none of the new variants; old builds already expect unknown variants to be unreadable, same policy as the `seq` migration per `events.rs:106-109`).

Highest-risk change is `SessionDetail` `LoadState` (~1 file) with possible `insta` snapshot churn. All other changes are 0-blast outside their edited surfaces.

## Rollout

One spec, two PRs:

1. **PR 1 — Tranche 1 (server correctness):** the two `BrainError` emission fixes and the two `guard.await` timeouts. Independently revertible. Fixes the actual hang at the orchestrator layer.
2. **PR 2 — Tranche 2 (UX redesign):** picker/SessionDetail refactor plus the milestone event additions. Depends on PR 1 for end-to-end correlation.

Separate follow-up beads issues (out of scope, tracked separately):

- `app.rs:1564` — `try_send(UserInput::ResumeSession)` silent-drop handling.
- `native.rs:478` — ACP `reply_rx.await` 5s timeout.

## Open questions

None. The design is grounded; timeout values are re-used from existing constants (`MCP_SHUTDOWN_TIMEOUT = 5s`), no new magic numbers are introduced, and precedents exist in the codebase for every new construct.
