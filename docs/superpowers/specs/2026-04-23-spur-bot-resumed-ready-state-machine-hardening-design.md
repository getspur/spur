# SPUR Bot Resumed-Ready State Machine Hardening Design

**Date:** 2026-04-23
**Status:** Proposed
**Scope:** `spur-bot` runtime resumed binding, routing cleanup, regression coverage
**Builds On:** [2026-04-22-spur-bot-telegram-thread-sessions-design.md](./2026-04-22-spur-bot-telegram-thread-sessions-design.md)
**Related RCA:** [2026-04-22-telegram-thread-sessions-pending-resume-leak.md](../../rca/2026-04-22-telegram-thread-sessions-pending-resume-leak.md)

---

## Problem Statement

The current resumed-session binding path in `crates/spur-bot/src/runtime.rs`
has two coupled correctness problems:

- `/resume <Y>` in a topic that is already `RestorePending { X }` does not evict
  stale `pending_resume` entries for that same topic.
- `AgentSessionReady { resumed: true }` commits through a permissive lookup path
  that can bind a topic without proving that the topic still expects that ACP
  session.

Together, those allow late resumed-ready events to violate operator intent and
leave stale routing state behind even when the visible binding eventually
recovers.

The design goal is to harden the runtime state machine without changing the ACP
protocol or adding orchestrator-side cancellation behavior.

---

## Product Goal

Preserve the thread-oriented Telegram session model while making resumed binding
obey one strict rule:

- a resumed-ready event may activate a topic only when that topic is still the
  current pending target for the same `acp_session_id`

The fix must also guarantee that:

- stale resumed-ready events do not bind the lobby
- stale resumed-ready events do not overwrite a newer `/resume` target
- stale session-to-thread routes are removed when a topic commits to a new live
  session

---

## In Scope

This design covers the primary P1 bug and its directly coupled fallout:

1. same-topic `/resume X` then `/resume Y` supersession behavior
2. resumed-ready validation before commit
3. `session_threads` cleanup when a topic commits to a new active session
4. regression coverage for resumed ready arrival orderings and route cleanup

---

## Out Of Scope

This design does not attempt to solve:

- upstream cancellation of superseded `ResumeSession { X }`
- operator-visible warnings for dropped stale resumed events
- `pending_inputs` multi-message buffering semantics
- deleted-topic send failure behavior
- lobby/topic command-surface cleanup unrelated to resumed binding
- unrelated lower-severity RCA items that need separate design or verification

Those remain valid backlog items, but they are not required to close the P1
resumed-binding bug safely.

---

## Constraints

The design must preserve the existing SPUR and Telegram-thread invariants from
the approved thread-session design:

- `AgentSessionReady` remains the only commit point for activating a binding
- the lobby never owns a live brain binding
- each live topic owns at most one live session binding
- no ACP session may be live-bound to multiple topics simultaneously

The design must also stay local to `spur-bot` runtime behavior:

- no ACP event schema changes
- no new orchestrator command types
- no persistent-state schema changes

---

## Approaches Considered

### 1. Minimal Patch

Patch only the two obvious sites:

- evict same-topic `pending_resume` entries before inserting a new one
- add a guard before committing `AgentSessionReady { resumed: true }`

This is attractive for patch size, but it leaves the live-route replacement rule
implicit and easy to regress.

### 2. State-Machine Hardening

Keep the external contract unchanged, but make resumed binding pass through
explicit local transition helpers:

- supersede pending resume intent
- resolve resumed ready only from exact pending intent
- bind active session through one routing-aware commit function

This is the approved approach because it closes the bug and the coupled routing
fallout with clear local invariants.

### 3. Structure Change

Replace `pending_resume: HashMap<String, ThreadKey>` with a broader per-topic or
bidirectional pending structure.

This is conceptually cleaner but is a larger refactor than the current incident
needs, and it increases test and migration churn without changing the external
behavior.

---

## Approved Approach

Use **state-machine hardening** inside `crates/spur-bot/src/runtime.rs`.

The runtime keeps the existing ACP and Telegram interfaces, but resumed binding
is rewritten around three explicit local responsibilities:

1. `supersede_pending_resume(key)`
2. `resolve_resumed_ready(acp_session_id) -> Option<ThreadKey>`
3. `bind_active_session(key, session, acp_session_id, brain)`

This gives the resumed path one clear rule:

- resumed ready events commit only through current pending topic intent, never
  through fallback heuristics

Fresh-session FIFO selection remains unchanged, but once that branch resolves a
target topic it should commit through the same `bind_active_session(...)`
helper. "Leave the fresh-session FIFO path unchanged" refers to how the topic is
selected, not to maintaining two separate commit blocks.

---

## Runtime Architecture

### `supersede_pending_resume(key)`

This helper runs before `/resume Y` inserts a new pending target for a topic.

Responsibility:

- remove all `pending_resume` entries whose value is `key`

Invariant enforced:

- one topic may have at most one in-flight pending resume target in runtime
  state

This mirrors the already explicit stale-eviction pattern used by
`pending_new_session_guard` for fresh sessions.

### `resolve_resumed_ready(acp_session_id)`

This helper is the only resumed-event resolution path.

Responsibility:

- remove `pending_resume[acp_session_id]`
- confirm the resolved topic still exists
- confirm the topic is still `RestorePending { acp_session_id }`
- return `Some(key)` only for an exact match
- otherwise return `None`

Invariant enforced:

- a resumed-ready event is actionable only if the runtime still has current
  topic intent for that exact ACP session

### `bind_active_session(key, session, acp_session_id, brain)`

This helper becomes the single commit point for live binding.

Responsibility:

- read the topic's current `live_session` before overwriting it
- remove any old `live_session` route owned by that topic from
  `session_threads` before writing the new live session
- set `BindingState::Active`
- set `record.live_session`
- update `record.acp_session_id`
- update `record.brain`
- insert the new `session_threads[session] = key`
- treat an existing `session_threads[session]` entry for a different topic as
  an invariant violation; implementations may defensively replace it, but must
  at minimum make that collision impossible to ignore during development

Invariant enforced:

- a topic exposes at most one live session route after commit

---

## Event Flow

### `/resume Y` While Topic Is `RestorePending { X }`

The runtime flow is:

1. ensure the topic record exists
2. archive any conflicting other-topic owner of `Y` exactly as today
3. call `supersede_pending_resume(key)` for the current topic
4. update the topic record to `RestorePending { Y }`
5. preserve the current pending-input behavior
6. send `ResumeSession { Y }`
7. insert only `"Y" -> key` into `pending_resume`
8. persist state

This makes same-topic `/resume` an explicit supersession of prior pending intent.

### `AgentSessionReady { resumed: true }`

The resumed-ready branch becomes:

1. call `resolve_resumed_ready(acp_session_id)`
2. if it returns `None`, drop the event with no render and no state mutation
3. if it returns `Some(key)`, call `bind_active_session(...)`
4. persist state
5. emit the normal restored service message

The current fallback behavior must not be used for stale resumed events:

- no fallback to `session_threads`
- no fallback to lobby binding

Those fallback paths are correct for other event families, but not for
superseded resumed-ready events.

### `AgentSessionReady { resumed: false }`

The fresh-session path keeps its current FIFO topic-selection semantics.

After that selection resolves a topic, it should also call
`bind_active_session(...)` rather than maintaining a separate inline commit
block. This keeps route replacement logic centralized and prevents the resumed
and fresh paths from drifting apart.

---

## Failure Handling

This design intentionally treats stale resumed-ready events as superseded
protocol noise.

If a resumed-ready event is dropped because:

- there is no pending target, or
- the topic no longer expects that ACP session

the runtime will:

- not mutate topic state
- not mutate `session_threads`
- not emit a user-facing warning
- not attempt upstream cancellation

This keeps the runtime strict and local. The authoritative operator intent is
the latest `/resume`, not the latest event arrival.

This also applies to restart races. `pending_resume` remains runtime-only
state; a post-restart late `AgentSessionReady { resumed: true }` with no current
pending target is intentionally ignored. Recovery comes from the next topic
message re-triggering `ResumeSession`, not from restoring the old fallback path.

---

## Testing Strategy

Regression coverage belongs in
`crates/spur-bot/tests/runtime_flow.rs` alongside
`stale_fresh_ready_does_not_reactivate_rebound_topic`, because the resumed bug
is the direct analogue of an already-known stale fresh-ready class.

Required coverage:

### 1. `old_ready_then_new_ready`

Setup:

- topic starts `RestorePending { X }`
- operator issues `/resume Y`
- deliver `AgentSessionReady(X)` first, then `AgentSessionReady(Y)`

Assertions:

- after stale `X`, topic remains `RestorePending { Y }`
- after stale `X`, topic has no `live_session`
- after `Y`, topic becomes `Active { Y }`

### 2. `new_ready_then_old_ready`

Setup:

- topic starts `RestorePending { X }`
- operator issues `/resume Y`
- deliver `AgentSessionReady(Y)` first, then stale `AgentSessionReady(X)`

Assertions:

- final state remains `Active { Y }`
- stale `X` does not overwrite the topic
- stale `X` does not create a second live route

### 3. `no_pending_target_drop`

Setup:

- deliver `AgentSessionReady { resumed: true }` after the pending target has
  already been cleared or superseded

Assertions:

- no topic binding changes
- no lobby attach occurs
- no `session_threads` route is created for the stale session

### 4. `route_cleanup_on_rebind_commit`

Setup:

- use the same-topic supersession ordering where stale `X` binds first and the
  later valid `Y` bind must replace that live route without an intervening
  `/resume`

Assertions:

- the old live session route no longer points to the topic
- the new live session route does point to the topic
- event routing keyed by the stale session id no longer reaches the topic
- event routing keyed by the committed new session id does reach the topic

The last case is mandatory. The coupled fallout is not only visible binding
corruption; it is also stale route retention.

---

## Acceptance Criteria

The implementation is correct only if all of the following are true:

- `/resume Y` in a topic supersedes any prior pending resume target for that
  same topic
- resumed-ready events only commit through an exact pending-topic match
- stale resumed-ready events cannot bind the lobby
- stale resumed-ready events cannot overwrite a newer `/resume` target
- when a topic commits to a new live session, stale `session_threads` routes are
  removed
- regression tests cover both resumed-ready arrival orderings and route cleanup

---

## Sequential Execution Shape

The user requested sequential handling per in-scope RCA issue with explicit
worker roles.

The implementation workflow should therefore decompose into sequential slices:

1. **Issue justification**
   - worker: `claude-code`
   - purpose: justify each in-scope RCA issue boundary and confirm why it is in
     or out of this design slice

2. **Implementation**
   - worker: `codex`
   - purpose: apply one bounded runtime/test slice at a time

3. **Review**
   - worker: `kimi`
   - purpose: review each completed slice against this design and the RCA

Recommended slice order:

1. pending-resume supersession helper
2. resumed-ready resolution guard
3. active-bind route cleanup helper
4. regression tests

This ordering keeps the state-machine changes readable and minimizes the chance
that tests encode transitional bugs.

---

## Risks And Tradeoffs

### Risk: Hidden Dependency On Current Fallback Behavior

Some existing edge path might currently rely on resumed events falling back to
`session_threads` or lobby routing.

Mitigation:

- keep the strict drop behavior limited to `resumed = true`
- leave the fresh-session FIFO path unchanged
- add regression tests around no-pending-target behavior

### Risk: Overfitting To One Incident Ordering

If tests only pin one arrival order, the opposite ordering can still regress.

Mitigation:

- require both orderings explicitly in acceptance criteria

### Tradeoff: No Operator Warning For Dropped Stale Events

This design prefers runtime containment over user-visible noise.

Why this is acceptable:

- stale resumed-ready events are internal superseded protocol artifacts
- the correct operator-visible surface is the final binding, not every dropped
  stale event

---

## Open Follow-Ups

These are intentionally not part of this design, but should remain visible:

- `pending_inputs` currently behaves as single-slot overwrite during restore
- lower-severity RCA items need separate verification or design treatment
- if resumed supersession becomes common, telemetry or debug logging may be
  worth adding later
