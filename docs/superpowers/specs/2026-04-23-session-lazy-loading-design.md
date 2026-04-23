# Session Lazy Loading Design

Date: 2026-04-23
Area: `crates/spur-tui`, `crates/spur-core`
Status: Approved for planning
Related:
- `docs/superpowers/specs/2026-04-12-tui-session-picker.md`
- `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md`

## Summary

The best lazy-loading approach is not to start with `SessionPickerView`
pagination in the TUI. The highest-value change is to make
`SessionDetailView` history hydration incremental while keeping picker work
structurally ready for true pagination later.

Recommendation:

1. Make resumed session history load incrementally into `SessionDetailView`.
2. Represent unloaded older history explicitly in the trace UI.
3. Introduce an explicit app/orchestrator contract for loading older history
   on demand rather than assuming the current resume path already supports it.
4. Refactor picker state so a future paginated backend can slot in cleanly,
   but do not ship fake picker lazy loading that still eagerly fetches the
   full session list.

This is the best first move because it reduces real resume-time work rather
than only reducing how much already-fetched data is painted.

## Problem

SPUR has two distinct session surfaces:

- `crates/spur-tui/src/views/session_picker.rs`
- `crates/spur-tui/src/views/session_detail.rs`

They do not have the same performance shape.

### Picker today

The picker already lazy-instantiates as a view, but once opened it eagerly
requests the full `Vec<SessionInfo>` via `ListSessions` and stores it locally.
Filtering, sorting, cursor movement, preview rendering, and visible-count
queries all assume a complete in-memory list.

Without a paginated list API, picker-side "lazy loading" in the TUI is mostly
row virtualization. That can reduce render work, but it does not reduce:

- backend session-list fetch cost
- connection cold-start cost
- memory for the fetched `Vec<SessionInfo>`

### Detail today

The detail view already has a shell-before-history lifecycle. `App` creates
`SessionDetailView` on `BrainSpawned`, and only then replays history via
`AgentNotification` replay or the `SessionHistory` disk fallback.

That means detail construction itself is not the eager hotspot. The eager
hotspot is full-history hydration into `ReactTrace`.

Current implications:

- resuming a long session can eagerly append a large transcript before the
  user needs the whole thing
- `ReactTrace` caches and scroll state are built around a complete replay
  arriving immediately
- the current disk replay path prepends and appends sentinel separators around
  the entire historical transcript rather than a bounded window
- the current `ResumeSession` path drains the resume history source to
  completion before control returns to normal view interaction

## Goals

- Reduce real resume-time work for long sessions.
- Preserve current resume semantics: shell first, history next, live session
  after that.
- Keep `ReactTrace` ordering, scroll-anchor behavior, and cache invalidation
  coherent under partial history.
- Make the lazy-history contract explicit at the `Action`/`UserInput`/
  `SpurEvent` boundary instead of burying it inside view-local behavior.
- Prepare the picker for future pagination without forcing protocol changes
  into this first phase.
- Keep the initial implementation bounded enough to plan and test clearly.

## Non-Goals

- Do not redesign the entire ACP session-list protocol in the first step.
- Do not add speculative picker-side virtualization that pretends to be lazy
  loading while still fetching everything eagerly.
- Do not claim that the existing `load_session` history stream already supports
  recent-first or random-access replay.
- Do not change session-switch safety around drafts, metadata preview, pinning,
  or archiving.
- Do not replace `ReactTrace` with a new generalized virtualized transcript
  engine in this phase.

## First-Principles Decision

Lazy loading is justified only if it removes real work from the critical path.

From first principles, the relevant costs are:

- connection and request latency
- transcript replay and parsing cost
- trace cache construction and scroll-state maintenance cost
- TUI render cost
- correctness risk introduced by partial state

Measured against those costs:

- picker-only TUI virtualization reduces only the last item
- detail incremental hydration reduces transcript replay, cache construction,
  and initial render work on resume

So the best first lazy-load target is `SessionDetailView`.

## Evaluated Approaches

### A. Picker-first TUI virtualization

Description:
- Keep `ListSessions` unchanged.
- Store the full list locally.
- Render only a visible slice and maybe defer preview construction.

Pros:
- TUI-only change.
- Lowest immediate risk.
- Helps if the row count itself is the main issue.

Cons:
- Not true lazy loading.
- Does not reduce backend work or fetch latency.
- Leaves all picker semantics coupled to a fully materialized list.

Decision: not the primary recommendation.

### B. Detail-first incremental history hydration

Description:
- Resume into a live `SessionDetailView` shell immediately.
- Hydrate a bounded history window first.
- Load older history on demand through a new explicit app/orchestrator path.

Pros:
- Attacks real work on the critical path.
- Aligns with the current `BrainSpawned` then history-replay lifecycle.
- Improves perceived resume latency for long sessions.

Cons:
- Requires explicit trace semantics for unloaded older history.
- Requires a new contract for loading older history after initial resume.
- Must preserve scroll-anchor, cache, and live-stream ordering invariants.

Decision: recommended first phase.

### C. Combined strategy in two phases

Description:
- Phase 1: detail-first incremental hydration.
- Phase 2: picker state refactor and eventual backend pagination.

Pros:
- Highest long-term value.
- Keeps near-term scope bounded.
- Avoids premature protocol churn while not painting the picker into a corner.

Cons:
- Requires discipline to stop after the first phase rather than mixing both.

Decision: recommended program shape.

## Proposed Design

### 1. Detail history becomes explicitly incremental

Resumed sessions should open into one of these states:

1. shell created, no history yet
2. initial history window loaded
3. older history partially loaded
4. full reachable history loaded

The user-visible mental model is:

- useful context appears quickly
- older context is available, but not free
- the trace tells the truth about what is and is not loaded

### Trace contract

`ReactTrace` must stop assuming that "history replay already gave me the entire
past."

Instead the session-detail stack needs an explicit boundary object,
conceptually:

```rust
enum HistoryLoadState {
    NotLoaded,
    Partial { older_remaining: usize },
    Complete,
}
```

This is the required state model, even if the concrete storage location or
final type names differ.

### Ownership model

The partial-history boundary should be owned by `SessionDetailView`, not folded
implicitly into the existing append-only `ReactTrace`.

Why:

- `ReactTrace` today is a `Vec<TraceEntry>` with append semantics, front
  eviction, and row-anchor bookkeeping
- the view already owns session-scoped policy decisions such as banners,
  input-state interactions, and history replay entry points
- a view-owned boundary keeps the first phase from requiring a full
  `ReactTrace` storage rewrite before the behavior is proven

Practical shape for phase 1:

- `SessionDetailView` owns `HistoryLoadState`
- `SessionDetailView` decides whether to render a sentinel row above the
  loaded trace
- `ReactTrace` remains the container for currently-loaded trace entries only
- `ReactTrace` still needs an explicit prepend-oriented surface in phase 1,
  because row anchors and cache invalidation live there

### Sentinel row

When older history exists but is not loaded, `SessionDetailView` should render
a visible sentinel row above the loaded history window, for example:

`--- Older history not loaded. PageUp to load more ---`

Why a sentinel row instead of silent truncation:

- it preserves truthful UX
- it gives scroll behavior a concrete boundary
- it avoids users mistaking a partial history window for a full transcript

### Initial window policy

The spec should not promise "recent-first" for every history source in phase 1.
The current live `load_session` contract exposes a forward replay stream of
historical notifications and may publish replay through the session
notification broadcast when pre-subscribed; it does not provide random access
or reverse pagination today.

So the grounded policy is:

- the first phase must support a bounded initial window
- that window may be "recent-first" only when the history source supports it
- the design must state separately how live-agent replay and disk fallback are
  expected to supply that window

Phase-1 acceptable implementations:

- disk fallback can provide a recent-first window by slicing the loaded
  `Vec<HistoryEntry>` before replay
- live replay can provide a bounded initial window only if the orchestrator
  explicitly buffers and trims before first paint, or if a new paged history
  contract is introduced

### On-demand expansion

Older history should load through an explicit action, not implicit surprise
background growth while the user is reading.

Primary trigger for the first phase:

- `PageUp` or equivalent upward scroll while the viewport is already pinned at
  the unloaded-history sentinel requests the next older chunk

But that trigger is only the UX surface. It is not sufficient by itself.

The implementation contract must add a real fetch path, for example:

- new `Action::LoadOlderHistory { session, cursor }`
- forwarded as a new `UserInput` variant
- mapped by the CLI bridge into a new `InteractiveInput` variant
- handled by the orchestrator through a retained history provider or a new
  paged replay source
- emitted back through a new `SpurEventBody` chunk event rather than
  overloading the existing one-shot `SessionHistory` semantics

Without that explicit contract, the current app treats scroll actions as
view-local no-ops after `handle_key`, and the orchestrator drains resume
history to completion during `ResumeSession`.

### 2. Orchestrator/history source must support bounded replay

The TUI cannot do true detail lazy loading if resume only exposes "all history
or nothing."

The orchestration side therefore needs a bounded-history concept for resumed
sessions. That can come from:

- chunked history stream consumption from the live agent transport, if the
  orchestrator retains or pages it explicitly
- chunked disk replay for fallback history
- or both

The design does not require the first phase to add a brand-new ACP primitive if
the orchestrator can internally bound replay from existing sources. But the
resulting contract to the TUI must behave like pages or chunks, not a single
monolithic dump.

### Live replay requirements

Because `load_session` currently yields or publishes a forward replay stream,
the orchestrator must make one of these choices explicitly:

1. Buffer the complete replay, derive an initial bounded window, and retain the
   remainder for later chunk delivery.
2. Extend the resume/history contract so the source itself can supply paged or
   cursor-based history.

The spec should not assume option 2 already exists.

If `LoadOutcome` influences which path is chosen, the implementation plan must
state whether that signal is merely advisory or part of the real lazy-history
decision contract.

### Disk fallback requirements

The existing `SessionHistory` disk replay path in
`SessionDetailView::replay_history` is all-or-nothing and wraps the full replay
in header/footer separators.

Grounded phase-1 scope:

- disk fallback may be the first history source to support true recent-window
  loading
- if disk fallback remains all-or-nothing in phase 1, the spec must say so
  explicitly rather than implying chunk-aware disk semantics already exist

If disk fallback is made incremental, that requires a concrete policy change at
the event layer, because persisted `HistoryEntry` currently contains only
`role` and `text`.

### History event contract

The current `SpurEventBody::SessionHistory { session, entries }` shape behaves
as a one-shot replay payload and triggers side effects in `App` beyond painting
the trace.

Incremental history should therefore use either:

- a new chunked event variant
- or a versioned extension of `SessionHistory` carrying chunk semantics

The event contract must answer:

- is this the initial history window or an older prepend chunk
- is more older history available afterward
- should app-level side effects such as input-history backfill run for every
  chunk or only for the initial hydrate
- which layer owns the full request/response chain:
  `Action -> UserInput -> InteractiveInput -> SpurEventBody`

### 3. App-side side effects must stay correct under chunking

Today the `SessionHistory` arm in `App` does more than call
`SessionDetailView::replay_history`:

- it backfills global input history from replayed user messages
- it persists metadata when that history changes
- it reseeds active input bars

Under chunked history, the design must define whether those side effects run:

- once for the initial hydrate only
- incrementally for every chunk
- or through a separate deduped aggregation path

The spec requirement is:

- chunking must not cause redundant churn or semantic regressions in global
  input-history recall

### 4. ReactTrace invariants that must not regress

Incremental history is only acceptable if these invariants hold.

### Ordering

Loaded older chunks must prepend in strict chronological order without
reordering existing live entries.

### Anchor stability

Prepending older entries must preserve the user's visible position relative to
the already-loaded content. This is the same class of problem already handled
carefully in the scroll-anchor work; incremental history must reuse that rigor.

That means phase 1 should expect a `ReactTrace` API in the shape of
`prepend_entries` or equivalent, with the opposite anchor bookkeeping from
front eviction:

- existing `ScrollAnchor::Row { entry_idx, .. }` values shift by `+N`
  prepended entries
- cache invalidation starts at the prepend boundary
- `Following` stays `Following`

### Cache invalidation

`ReactTrace` line caches must invalidate from the prepend boundary correctly.
No stale row counts, no accidental jump to bottom, no corruption of scroll
coordinates.

### Capacity behavior

`ReactTrace` still enforces `MAX_LOG_ENTRIES = 5_000`. Incremental loading must
have a defined interaction with that cap.

Required policy:

- loading older history must not silently evict the most recent live context
  the user just resumed into
- if a bounded transcript window is necessary, recency wins over remote past

That implies the design should prefer an explicit loaded-history budget over
naively prepending until the generic trace eviction logic fires.

### 5. Picker is refactored for future pagination, not fake laziness now

`SessionPickerView` should be restructured so it can adopt real pagination
later, but phase 1 should not claim to solve picker lazy loading if it still
eagerly fetches the full list.

### State split

Today picker state couples:

- canonical session data
- filtering and sorting
- rendered window
- preview derivation

The future-proof refactor is to separate:

- canonical loaded sessions
- filter/sort projection
- viewport window
- highlighted-row preview model

That refactor is worth doing because it makes later backend pagination much
cleaner. But by itself it is not the main performance win.

### True picker lazy loading prerequisite

Real picker lazy loading should wait for one of:

- paginated `ListSessions`
- cursor-based listing
- a dedicated summary/preview endpoint

Until then, picker work is a structural cleanup, not the lead optimization.

## Data Flow

### Resume with incremental history

```text
User selects session in picker
  -> Action::ResumeSession
  -> Orchestrator resumes session
  -> SpurEvent::BrainSpawned
  -> App constructs SessionDetailView shell
  -> Orchestrator emits initial history window event
  -> SessionDetailView renders current loaded context + older-history sentinel
  -> User presses PageUp at sentinel
  -> Action::LoadOlderHistory
  -> UserInput::LoadOlderHistory
  -> InteractiveInput::LoadOlderHistory
  -> Orchestrator emits older-history chunk event
  -> SessionDetailView prepends that chunk without moving the viewport
  -> Live notifications continue below the loaded history boundary
```

### Picker in phase 1

```text
User opens picker
  -> Action::RequestSessions
  -> UserInput::ListSessions
  -> Orchestrator fetches full session list as today
  -> SessionPickerView stores canonical list
  -> View renders a derived window only
```

This is intentionally not marketed as full lazy loading.

## Acceptance Criteria

- Resuming a long session produces visible context before the full transcript
  would have completed under the old design.
- The trace clearly indicates when older history is not yet loaded.
- Loading older history does not reorder live entries or yank the viewport.
- Scroll-anchor behavior remains stable when older chunks are prepended.
- The event contract for lazy history is explicit enough that the behavior does
  not depend on hidden orchestrator state.
- The full request/response chain is specified:
  `Action -> UserInput -> InteractiveInput -> SpurEventBody`.
- App-level input-history side effects remain correct under chunked history.
- Disk fallback semantics are truthful about whether they are partial or full.
- The live `load_session` replay path and the disk-fallback path are both
  covered by acceptance criteria and tests, not just "history" generically.
- Picker behavior is unchanged for users in phase 1, except for any internal
  state refactor required to prepare for future pagination.

Suggested proof harnesses:

- `crates/spur-tui/src/components/react_trace/streaming_tests.rs` for
  prepend/anchor/cache behavior
- session-resume tests spanning both live replay and `SessionHistory` fallback

## File Impact

Expected primary impact:

- `crates/spur-tui/src/views/session_detail.rs`
- `crates/spur-tui/src/components/react_trace/*`
- `crates/spur-tui/src/app.rs`
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-acp/src/domain/events.rs`
- `crates/spur-tui/src/action.rs`

Expected secondary impact:

- `crates/spur-tui/src/views/session_picker.rs`
- relevant TUI/orchestrator tests for session resume and picker behavior

## Risks

- Partial-history semantics can introduce subtle scroll bugs if prepend
  invalidation is not designed upfront.
- A poor loaded-history budget can fight `MAX_LOG_ENTRIES` and accidentally
  discard the most valuable recent context.
- If history chunks are not well-defined at the orchestrator boundary, the TUI
  will end up re-implementing transport concerns it should not own.
- If picker refactoring and detail hydration are mixed in one change, the
  review surface gets wider without improving the core outcome.
- If the spec over-promises recent-first behavior without a matching history
  source contract, implementation will either buffer too much or silently drift
  from the doc.

## Recommendation

Plan the work in this order:

1. Introduce the lazy-history action/event contract and choose the retained
   history-source model.
2. Implement detail-first incremental history hydration against that contract.
3. Add trace/sentinel/viewport invariants and tests.
4. Decide whether disk fallback participates in phase 1 or remains
   all-or-nothing temporarily.
5. Do picker state cleanup for future pagination.
6. Add backend pagination or preview-summary support for the picker only after
   the detail path is complete and measured.

If the team can only choose one immediate investment, it should be
`SessionDetailView` incremental history hydration.
