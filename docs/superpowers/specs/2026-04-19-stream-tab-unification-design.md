# Stream Tab Unification — One Builder, Two Views

**Date:** 2026-04-19
**Scope:** `crates/spur-tui/src/components/detail_pane.rs`, `crates/spur-tui/src/components/react_trace/`, `crates/spur-tui/src/app.rs`, `crates/spur-core/src/lineage/projection.rs` (behavior narrowing, no schema change)
**Status:** Draft — approved for plan authoring
**Related docs:**
- `docs/superpowers/specs/2026-04-13-executor-lineage-visualization-design.md`
- `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md`
- `docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md`

## Problem

SPUR currently renders worker/executor streams in two structurally unrelated
paths that were designed to show the same thing:

- **Brain session view** (`crates/spur-tui/src/views/session_detail.rs`)
  consumes the live `SessionNotification` feed via
  `crates/spur-tui/src/components/react_trace/builder.rs`, producing
  `TraceEntry { kind ∈ Think | AgentMessage | Act{tool, family, input, status}
  | Observe | Delegate | UserMessage | Permission, ... }` rendered with tool-call
  lifecycle, spinners, optional markdown/mermaid, and row-precise
  `ScrollAnchor`.
- **DetailPane Stream tab** (`crates/spur-tui/src/components/detail_pane.rs`,
  `render_stream`) consumes `ExecutorNode.stream_buffer` — a
  `VecDeque<WorkerStreamEntry>` with three coarse kinds
  (`Thought | Message | ToolCall`) and renders one truncated line per block
  with a scalar scroll offset.

The upstream event already carries full ACP fidelity:

```rust
// crates/spur-acp/src/domain/events.rs:525
SpurEventBody::WorkerNotification {
    brain_session_id: SessionId,
    executor_id: String,
    notification: Box<SessionNotification>, // same type the brain view consumes
}
```

The divergence happens at exactly one point — the match arm in
`crates/spur-core/src/lineage/projection.rs:274–316` — which downsamples
`SessionNotification` into `WorkerStreamEntry` and drops every update the old
renderer could not use (`ToolCallUpdate`, `Plan`, `Permission`,
delegation signals).

Consequences today:

1. The Stream tab cannot show tool-call lifecycle, permission prompts,
   delegate cards, or observe payloads.
2. Every new `SessionUpdate` variant must be handled in three places
   (`react_trace::builder`, `lineage/projection`, and the implicit coupling
   inside `DetailPane::render_stream`) — or silently disappears from the
   Stream tab.
3. The product claim that the Stream tab mirrors the brain view is
   structurally false: the two paths share neither input type nor code.

## Goals

1. The Stream tab and the brain view render from the **same input type**
   (`SessionNotification`) through the **same builder** (`react_trace/builder.rs`).
2. Every future ACP `SessionUpdate` variant added to the brain view appears
   automatically in the Stream tab with no duplicate plumbing.
3. The rollout is strictly additive: zero serde-format change, zero
   `SessionHistory` migration, every phase independently revertable.
4. Visual density in the narrow detail pane (~40 columns) is preserved via a
   new `compact` render mode on `ReactTrace`, not by a separate renderer.
5. The workers-panel card (`inline_executor_card`) continues to work
   unchanged; `ExecutorNode`'s serde surface is unchanged.

## Non-goals

- Embedding `SessionDetailView` in the pane. The pane does not own a chat
  input, picker-shell, auth banner, resume banner, or completion popup.
- Pushing `TraceEntry` or `ReactTrace` into `spur-core`. Rich TUI types
  stay TUI-side.
- Deleting or upgrading `stream_buffer` in this change. The projection
  continues to write it; its role narrows to card summary only. A future
  change may retire it, deferred indefinitely.
- Changing the ACP wire protocol or `SpurEventBody` enum.
- Enabling markdown or mermaid rendering inside the compact pane.
- Replacing the brain view's full render path with the compact path.

## Architecture

### Component map (after cutover)

```
SpurEvent stream
  ├── LineageProjection  ──► ExecutorNode summary (counters, diff, phase, last_error)
  │                          └── stream_buffer (card summary only; optional)
  │
  └── App::route_worker_notification(executor_id, notification)
         └── per_executor_traces: LruCache<ExecutorId, ReactTrace>
                └── ReactTrace (built by react_trace/builder.rs)
                       ├── compact: true  ──► DetailPane::render_stream
                       └── (brain view uses compact: false for full fidelity)
```

`react_trace/builder.rs` becomes the **single interpretation** of a
`SessionUpdate`, consumed by both call sites.

### Per-executor trace ownership

- Lives on the TUI layer (tentatively on `App`; may be hoisted to a
  dedicated `WorkerStreams` struct if `App` grows unwieldy).
- Keyed by `ExecutorId`.
- Bounded by an LRU (default `N = 8`) plus the currently-focused executor
  pinned. On eviction, the `ReactTrace` is dropped entirely.
- Cold lookup (first focus) triggers an on-demand replay: iterate the
  persisted `WorkerNotification` events for that `executor_id` through the
  builder to reconstruct the trace. If the event slice has been trimmed,
  seed from `stream_buffer` as a best-effort preamble.

### Compact render mode on `ReactTrace`

- New field `compact: bool` (default `false`) set at construction.
- `render()` branches early when `compact`:
  - One row per entry.
  - Glyph + width-truncated text + right-aligned `Ns ago`.
  - Markdown/mermaid paths skipped; `MarkdownStream` not constructed.
  - Separator row on kind transition (as today).
  - `ScrollAnchor::Row` still supported; `Following` still the default.
- All existing machinery (entries, line_cache, dirty_from, generation,
  tool_depth) is reused. Compact mode is a **render branch**, not a
  separate trace type.

### AgentKind derivation

`ExecutorNode.agent: String` must be mappable to `spur_acp::AgentKind` for
accent color and title. If `spur_acp` does not already expose a canonical
parser, add one in Phase 0. Default to `AgentKind::Generic` on unknown
names.

### Lineage projection narrowing

After Phase 3, `lineage/projection.rs:274–316` stops synthesizing
`WorkerStreamEntry`. The `match &notification.update` block reduces to:

- `ToolCall(tc)` — increment `tool_call_count`, update `latest_tool_call`,
  then stop (no `stream_buffer.push_back`).
- All other arms — update `last_event_at` only.

`WorkerStreamKind`, `WorkerStreamEntry`, and
`ExecutorNode.stream_buffer` remain declared and serde-serializable for
backward compatibility. They are simply no longer written from
`WorkerNotification`. Cards read `tool_call_count` / `latest_tool_call`
today; that continues to work.

> **Note.** If any current consumer of `stream_buffer` is discovered during
> Phase 0 audit, Phase 3 is deferred until that consumer is retargeted. The
> Phase 1–2 work stands independently.

## Phases

### Phase 0 — Landing pad (zero behavior change)

1. Add `compact: bool` field to `ReactTrace`; constructor `with_kind_compact`.
2. Implement `render_compact` in
   `crates/spur-tui/src/components/react_trace/render.rs`, gated by the flag.
3. Audit `stream_buffer` readers across the workspace. Document every one.
4. Ensure `spur_acp::AgentKind` has a `from_name(&str) -> AgentKind` (add if
   missing, default to `Generic`).
5. Golden tests for `render_compact` at widths 20, 40, 80 covering each
   `TraceKind`.

Deliverable: `ReactTrace` can render compact; brain view behavior
unchanged; audit complete.

### Phase 1 — Dark-launch per-executor traces

1. Add `per_executor_traces` on `App` with LRU capacity 8 + a pinned slot
   for the currently-focused executor.
2. In `App`'s `SpurEvent` handler, after the projection runs, call a new
   `route_worker_notification(executor_id, &notification)` that routes the
   `SessionUpdate` through the same builder entry points the brain view
   uses (`react_trace::builder::apply_session_update` or equivalent —
   exact symbol is an implementation decision made during planning; it
   must be the path already used by `SessionDetailView`).
3. On first focus of an executor, replay that executor's persisted
   `WorkerNotification` slice through the builder. If unavailable, seed
   from `stream_buffer` as a preamble.
4. No render path changes. Validate via debug dump / logs / unit tests.

Deliverable: traces populated in memory behind the scenes; nothing
renders from them yet.

### Phase 2 — Cutover

1. `DetailPane::render_stream` becomes: look up the focused executor's
   `ReactTrace` in `per_executor_traces`; if present, delegate to
   `render(compact: true)`; otherwise fall back to the pre-change
   `render_stream` reading from `stream_buffer`.
2. Keep the fallback branch live for **one** release cycle.
3. Visual parity assertion: for a representative recorded session, both
   paths produce substantially the same output for
   `Thought | Message | ToolCall` entries; additional fidelity
   (lifecycle, delegate, permission) appears only on the new path.

Deliverable: live Stream tab shows tool-call lifecycle, permissions,
delegate cards matching brain view fidelity.

### Phase 3 — Cleanup

1. Remove the fallback branch in `DetailPane::render_stream`.
2. Remove the `WorkerStreamEntry` push in `lineage/projection.rs`
   (counters/last_event_at survive). Keep the types declared for serde
   compatibility with existing `session_metadata.json` on disk.
3. Update docs: "`WorkerNotification` is the single source of truth.
   `stream_buffer` is retained for backward-compat only."

Deliverable: one render path, one builder, one source of truth.

### Phase 4 (deferred)

Delete `WorkerStreamEntry` / `WorkerStreamKind` / `stream_buffer` entirely
only if a future audit confirms no consumer remains. Bundled with a
`SessionHistory` format version bump if necessary. Not scheduled.

## Data Flow

### Today

```
SessionNotification
   │
   ▼
LineageProjection::apply
   │
   ├─► ExecutorNode.stream_buffer ◄── lossy (3 kinds)
   │        │
   │        ▼
   │   DetailPane::render_stream ── truncated 1-line rows
   │
   └─► (brain view) separately consumes the same notification via
       react_trace::builder.rs for the brain_session_id
```

### After Phase 3

```
SessionNotification
   │
   ├─► LineageProjection::apply — counters / diff / phase only
   │        │
   │        ▼
   │   ExecutorNode (summary)
   │        │
   │        ▼
   │   InlineExecutorCard
   │
   └─► App::route_worker_notification(executor_id, ...) ──────────┐
            │                                                       │
            ▼                                                       │
       react_trace::builder.rs  ──► per_executor_traces[eid]        │
                                       │                             │
                                       ▼                             │
                                   ReactTrace ──► DetailPane Stream  │
                                                  (compact: true)    │
                                                                     │
       (brain view) ─────► react_trace::builder.rs ──► SessionDetailView
                                                       (compact: false)
```

## Contracts

### `ReactTrace::render(area, compact)`

- Input: viewport `Rect`, `compact: bool`.
- Behavior:
  - `compact == false`: existing full render path (markdown, mermaid,
    multi-line entries, spinners).
  - `compact == true`: single-row per entry, no markdown/mermaid
    allocations, right-aligned relative timestamp, separator on kind
    transition, existing `ScrollAnchor` semantics preserved.
- Invariants:
  - `last_total_lines`, `last_visible_height`, `last_render_width` are
    updated identically in both modes.
  - Generation counter / dirty_from are invalidated consistently.
  - `tool_depth` still populated on `Act` entries; nesting may render as
    a leading indent glyph in compact mode (implementation detail).

### `App::route_worker_notification(executor_id, notification)`

- Preconditions: called **after** `LineageProjection::apply` for the same
  event.
- Behavior: look up or materialize `per_executor_traces[executor_id]`;
  feed `notification.update` to the shared builder.
- Materialization on cold lookup: replay persisted `WorkerNotification`
  events for `executor_id` through the builder; fall back to
  `stream_buffer` seeding if events unavailable.
- Eviction: LRU of size 8; currently-focused executor pinned.

### `LineageProjection::apply` (post-Phase-3)

- On `SpurEventBody::WorkerNotification`:
  - Updates `last_event_at`.
  - On `SessionUpdate::ToolCall`, increments `tool_call_count` and sets
    `latest_tool_call`.
  - Does **not** write to `stream_buffer`.
- Everything else is unchanged.

## Error Handling

- **Cold trace with no replayable events.** Materialize an empty
  `ReactTrace`; render the existing "(waiting for worker output…)"
  placeholder via the compact render path.
- **`AgentKind::from_name` miss.** Default to `AgentKind::Generic`; color
  + title fall back to the generic palette.
- **Builder error on malformed notification.** Builder already tolerates
  missing fields for the brain view; no new error paths needed.
- **LRU evicts a non-focused trace mid-event.** Subsequent events for that
  executor rematerialize on demand via replay. No data loss —
  `WorkerNotification` events remain in the event log.
- **Focused executor changes mid-render.** Re-entrancy is avoided because
  render happens on the main thread after event dispatch; a focus change
  takes effect next frame.

## Testing

### Phase 0

- Golden tests for `render_compact` at widths 20/40/80 covering each
  `TraceKind` (including `Act` with each `ActStatus`).
- Unit tests for `AgentKind::from_name` on every known agent plus
  unknown fallback.

### Phase 1

- Unit test: `route_worker_notification` for a fresh executor creates a
  `ReactTrace`, feeds an `AgentMessageChunk`, observes a single
  `TraceKind::AgentMessage` entry.
- Unit test: LRU evicts at capacity, pinned focused trace survives.
- Unit test: cold replay from a recorded `WorkerNotification` slice
  reconstructs a known entry count.

### Phase 2

- Snapshot test: recorded session renders identically (modulo new
  fidelity rows) under old and new paths for the `Thought | Message |
  ToolCall` subset.
- Regression test: `ToolCallUpdate` applied after a `ToolCall` advances
  `ActStatus` from `Pending`/`InProgress` to `Completed`/`Failed` in the
  compact trace.
- Regression test: `Permission` and `Delegate` entries appear on the
  Stream tab (they do not today).

### Phase 3

- Test: `LineageProjection::apply` does not push to `stream_buffer` on
  `WorkerNotification`.
- Test: counters (`tool_call_count`, `latest_tool_call`, `last_event_at`)
  still update correctly.

## Risk Register

| # | Risk | Mitigation |
|---|---|---|
| R1 | Persisted event log trimmed → focused cold executor replays nothing | Seed from `stream_buffer` as best-effort preamble |
| R2 | `AgentKind` parse fails on agent string | Default to `AgentKind::Generic`; Phase 0 adds parser |
| R3 | Projection vs. builder consume same event out of order | Single synchronous event-handling callback; projection runs first, builder second |
| R4 | Memory regression with 50+ executors | LRU cap (N=8) + full `ReactTrace` drop on evict; Phase 1 tests cover it |
| R5 | `tool_call_id` collisions across executors | Per-executor `ReactTrace` scopes the namespace naturally |
| R6 | Current consumer of `stream_buffer` not surfaced by Phase 0 audit | Defer Phase 3 write-path removal until consumer is retargeted; Phase 1/2 still land |
| R7 | Compact render cost exceeds today's `render_stream` in tight loops | Line cache + `dirty_from` amortize; if Phase 2 snapshot tests show regression, add a targeted benchmark before Phase 3 |
| R8 | Focus-change churn rebuilds traces repeatedly | Pin focused + LRU of 8 keeps the last few warm; churn bounded |

## Open Questions

None blocking. Implementation decisions deferred to the plan:

- Exact symbol used to feed `SessionUpdate` into the builder
  (`apply_session_update` vs. a new public entry point).
- Whether `per_executor_traces` lives on `App` or in a dedicated struct.
- Whether `AgentKind::from_name` is a free function or a `FromStr` impl.
- Compact-mode indentation glyph for subagent nesting.

## Acceptance Criteria

1. Stream tab shows tool-call lifecycle spinners and terminal glyphs
   identical to the brain view.
2. Stream tab shows delegate cards, permission prompts, and observe
   payloads that the old renderer dropped.
3. `SessionHistory` files written before this change load and render
   without modification.
4. `InlineExecutorCard` counters (`tool_call_count`, `latest_tool_call`)
   remain correct.
5. No new `unsafe` blocks, no new clippy warnings at `-D warnings`.
6. All existing tests pass; new tests from the Testing section pass.
