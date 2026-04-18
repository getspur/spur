# ReactTrace Act Status State Machine

## Goal

Fix the TUI spinner that never stops after a tool call succeeds, and eliminate
the latent inverse bug where streamed partial output prematurely stops the
spinner. Replace the fragile "adjacent `Observe` with `Some(payload)` stops the
spinner" rule with an explicit per-`Act` status field whose value is driven by
the ACP `ToolCallStatus` enum.

## Symptom

Users report that when the brain makes a tool call, the Braille spinner on the
collapsed `Act` line keeps animating after the tool has clearly completed.
With many tool calls in a session, many spinners animate concurrently and none
stop.

## Ground-Check Findings

Draft designs assumed facts about the codebase and the ACP schema. Some were
wrong. The design below reflects what is actually true:

1. **`ToolCallStatus` exists in the ACP schema** as
   `{ Pending, InProgress, Completed, Failed }` (default: `Pending`), but the
   TUI at `crates/spur-tui/src/views/session_detail.rs:1229` currently ignores
   it entirely — it derives "is the tool done" solely from
   `tcu.fields.raw_output.is_some()`. This is wrong in both directions:
   - `status: Completed` with `raw_output: None` never stops the spinner.
   - `status: InProgress` with streamed `raw_output: Some(partial)` would stop
     the spinner prematurely.
2. **`ToolCallId` is `Arc<str>`**, not `String`. Cheap to clone.
3. **`spur_acp::HistoryEntry`** at `crates/spur-acp/src/domain/events.rs:509`
   is `{ role: String, text: String }` only. It does not preserve
   `tool_call_id`. This is **not** a constraint because
   `replay_history` at `crates/spur-tui/src/views/session_detail.rs:488-512`
   only produces `UserMessage` and `AgentMessage` entries — it never
   constructs `Act`/`Observe` entries from history.
4. **`ObservePayload`** at `crates/spur-acp/src/adapter/mod.rs:73` derives
   `Debug, Clone` only. It is not `Serialize`. This is not a constraint
   because `TraceKind` has no serde derives (verified in
   `crates/spur-tui/src/components/react_trace/types.rs`) — trace entries are
   runtime-only UI state.
5. **`ToolCallUpdate` may arrive before `ToolCall`.** The ACP schema exposes
   `TryFrom<ToolCallUpdate> for ToolCall`, which is only meaningful if
   out-of-order arrival is possible. The design must tolerate it.
6. **Update-in-place is already established in this component.**
   `ReactTrace::attach_executor_id` at
   `crates/spur-tui/src/components/react_trace/mod.rs:615-635` scans backward
   through `entries` and mutates a `Delegate` entry's `executor_id` field in
   place, bumping `invalidate_cache`. The new design uses the same pattern.
7. The TUI is single-threaded. `&mut self` exclusion is sufficient — no
   shared-memory races.

## Root Cause

The spinner stop is encoded by adjacency in three renderers
(`builder.rs:54`, `mod.rs:660`, `mod.rs:839`) and the tick animator
(`mod.rs:469-486`):

> "Stop the spinner iff `entries[act_idx + 1]` is
> `TraceKind::Observe { payload: Some(_) }`."

Two push-side behaviours violate that contract.

**Cause A — multiple pushes per tool call.** The `ToolCallUpdate` handler at
`session_detail.rs:1226-1247` pushes a **new** `Observe` entry on every
update. ACP streams multiple updates per call (at minimum: one `InProgress`
with `raw_output: None`, then one `Completed` with `raw_output: Some(...)`).
The adjacency check at the `Act` always sees the first `Observe(None)`; the
real `Observe(Some(...))` sits further down the entry list and is never
paired with its `Act`.

**Cause B — informational `Observe { None }` pushes.** System notes
(`push_system_note`, `push_cancel_note`) and brain events (`BrainError`,
`BrainReconnecting`, `BrainReconnected`, `BrainReconnectFailed`) all push
`TraceKind::Observe { payload: None }`. Any that land between an `Act` and
its terminal update pin the spinner.

**Cause C — completion signal ignores `ToolCallStatus`.** Even if Cause A were
fixed to "one `Observe` per `Act` updated in place", the TUI would still
misread the state because it keys on `raw_output.is_some()` rather than on
the ACP status enum.

## Design

### Data model change

Rework `TraceKind::Act` to carry an explicit status. `Observe` remains but is
reserved for informational notes only (its tool-call role goes away).

```rust
// spur-tui/src/components/react_trace/types.rs
use spur_acp::ToolCallId;

pub enum TraceKind {
    Think,
    AgentMessage { agent: String },
    Act {
        tool: String,
        family: ToolFamily,
        input: ToolInputDisplay,
        tool_call_id: Option<ToolCallId>,
        status: ActStatus,
    },
    Observe { payload: Option<ObservePayload> }, // informational notes only
    Delegate { /* unchanged */ },
    UserMessage,
    Permission { /* unchanged */ },
}

pub enum ActStatus {
    Pending,
    InProgress { partial: Option<ObservePayload> },
    Completed(Option<ObservePayload>),
    Failed(Option<ObservePayload>),
}
```

Rationale:

- Sum types make illegal states unrepresentable. An `Act` either is running
  (`Pending` / `InProgress`) or it has terminated (`Completed` / `Failed`).
  The two cannot both be true, and neither can be absent.
- `ActStatus` mirrors `ToolCallStatus` 1:1. Mapping ACP → TUI is a direct
  match with no loss.
- Embedded payload removes the second-entry requirement, which removes the
  adjacency invariant and the three renderer branches that enforced it.
- `tool_call_id: Option<ToolCallId>` uses the upstream `Arc<str>` newtype
  directly. `None` is permitted but not produced by live ACP code; it
  protects future callers that may synthesize `Act` entries.

### Push-side mapping

At `session_detail.rs:1189` (`SessionUpdate::ToolCall`):

```text
push Act {
  tool, family, input,
  tool_call_id: Some(tc.tool_call_id.clone()),
  status: map_initial_status(tc.status, tc.raw_output),
}
```

The initial status honours `tc.status` rather than defaulting to `Pending`,
because an agent may stream an already-completed tool call in its first
event. `parse(v)` below denotes the existing
`spur_acp::adapter::extract_observe(v, kind)` call that produces
`ObservePayload`. `map_initial_status` logic:

| ACP status   | raw_output  | ActStatus                                |
|--------------|-------------|------------------------------------------|
| Pending      | any         | `Pending`                                |
| InProgress   | None        | `InProgress { partial: None }`           |
| InProgress   | Some(v)     | `InProgress { partial: Some(parse(v)) }` |
| Completed    | None        | `Completed(None)`                        |
| Completed    | Some(v)     | `Completed(Some(parse(v)))`              |
| Failed       | None        | `Failed(None)`                           |
| Failed       | Some(v)     | `Failed(Some(parse(v)))`                 |

At `session_detail.rs:1226` (`SessionUpdate::ToolCallUpdate`):

```text
if let Some((idx, act_entry)) = react_trace.find_act_by_id_mut(&tcu.tool_call_id):
    let new_status = merge_status(current_status, tcu.fields.status, tcu.fields.raw_output)
    set act_entry.status = new_status
    react_trace.mark_dirty_from(idx)
else:
    tracing::debug!("ToolCallUpdate for unknown tool_call_id {id}");
    if tcu.fields.title.is_some():
        synthesize a new Act entry from the update
        (status mapped from tcu.fields; tool_call_id preserved)
    else:
        drop
```

Merge rules for `merge_status(prev, incoming_status, incoming_raw_output)`:

- If `incoming_status` is `None`, keep the variant of `prev` but refresh its
  embedded payload when `incoming_raw_output` is `Some(v)` and `prev` is
  non-terminal (`InProgress.partial` is replaced).
- If `incoming_status` is `Some(s)`, map `(s, incoming_raw_output)` via the
  table above to produce the new variant. An incoming terminal
  (`Completed` / `Failed`) always replaces a non-terminal `prev`.
- If `prev` is already terminal (`Completed(_)` / `Failed(_)`),
  `debug_assert!` that the incoming status, if present, matches the terminal
  variant; in release builds log via `tracing::debug!` and keep `prev`
  unchanged. This prevents a late `InProgress` update from reopening a
  closed tool call.

### Renderer change

Spinner-vs-outcome is now a single match on `status`:

```rust
let active = matches!(status, ActStatus::Pending | ActStatus::InProgress { .. });
if active {
    // draw SPINNER_FRAMES[tick]
} else {
    // draw outcome glyph from Completed(_) / Failed(_)
    // body (if any) from the embedded payload
}
```

All three render paths change symmetrically:
`builder.rs:37-78` (markdown build),
`mod.rs:650-683` (plain-text `render_to_strings`),
`mod.rs:819-843` (plain-text Act with payload path).

The `consumed = 2` bookkeeping disappears — one entry represents one tool
call.

### Tick animator

`first_active_spinner` (`mod.rs:469-486`) becomes:

```rust
self.entries.iter().position(|e| matches!(
    &e.kind,
    TraceKind::Act { status: ActStatus::Pending | ActStatus::InProgress { .. }, .. },
))
```

No more neighbour probe. The `observe_collapsed` gate is removed from
`first_active_spinner` because the "active" predicate is now a property of
the `Act` entry itself, independent of render mode. The renderer keeps
`observe_collapsed` as a purely visual setting that controls whether the
outcome body is rendered inline; spinner vs. outcome-glyph selection is
driven entirely by `ActStatus`.

### Helper on `ReactTrace`

```rust
pub(crate) fn find_act_by_id_mut(&mut self, id: &ToolCallId) -> Option<(usize, &mut TraceEntry)>
```

Scans `self.entries` in reverse — O(N), bounded by `MAX_LOG_ENTRIES = 500`.
Returns the newest matching `Act`. Caller mutates `status` and invokes
`self.mark_dirty_from(idx)`. Mirrors `attach_executor_id`'s pattern.

### What stays the same

- `TraceKind::Observe` is retained and still used by
  `push_system_note`, `push_cancel_note`, and the brain-event handlers. Those
  no longer collide with tool-call lifecycle because tool-call terminal
  state now lives inside `Act`.
- `ScrollAnchor` semantics unchanged — mutation is in-place, no inserts or
  removes.
- `render_with_ctx`, `build_virtual_rows`, `line_cache` contracts unchanged.

## Out of Scope

- Renaming `TraceKind::Observe` to `TraceKind::Note`. A pure cosmetic
  follow-up once all tool-call uses are gone.
- Persisting `tool_call_id` across restarts via `HistoryEntry`. Requires an
  upstream schema change in `spur_acp`.
- A `HashMap<ToolCallId, usize>` index to replace the O(N) backward scan.
  Premature — `MAX_LOG_ENTRIES` is 500 and scans run at event cadence, not
  per-frame.

## Testing

New or updated tests in
`crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

1. `act_pending_shows_spinner` — build a trace with a single
   `Pending` Act; assert `first_active_spinner` returns `Some(idx)` and the
   rendered line contains a spinner frame.
2. `act_completed_stops_spinner` — transition Pending → Completed; assert
   `first_active_spinner` returns `None` and the rendered line contains the
   outcome glyph rather than a spinner character.
3. `in_progress_with_partial_keeps_spinner` — transition Pending → InProgress
   with `Some(partial)`; assert spinner is still active.
4. `completed_without_payload_stops_spinner` — transition Pending →
   `Completed(None)`; assert spinner stops. This is the exact bug the user
   reported.
5. `multiple_updates_mutate_in_place` — ToolCall followed by two
   ToolCallUpdate events; assert `entries.len()` stays constant after
   updates and the single `Act` reaches `Completed`.
6. `interleaved_system_note_does_not_affect_status` — push an `Act Pending`,
   push a `system_note` (which inserts `Observe { None }`), then push a
   terminal update; assert the `Act` still transitions to `Completed` and
   the spinner stops.
7. `out_of_order_update_logs_and_drops` — send a `ToolCallUpdate` before its
   `ToolCall`; assert nothing is pushed and a tracing event is emitted
   (captured via `tracing_test` if available; otherwise assert
   `entries.len() == 0`).
8. `failed_status_maps_to_failed_variant` — `Failed(Some(payload))` renders
   with the failure outcome glyph.
9. History replay unaffected — existing `replay_history` tests stay green
   because replay does not produce `Act` entries.

Existing tests that construct `TraceKind::Act { ... }` literals (roughly
`mod.rs:1193-1232` and `streaming_tests.rs`) get updated with the new fields.
Tests that assert adjacency-based rendering of the collapsed Act+Observe
pair are updated to assert status-based rendering of a single `Act`.

## Risks and Mitigations

- **Test-surface churn** — bounded: the affected sites are the ones that
  already construct `Act` or assert the collapsed-render shape. Estimated
  low double-digit count. Required anyway because the model is changing.
- **`TraceKind::Observe` dual purpose during the transition** — tolerable.
  Tool-call producers stop pushing Observe on Day 1; informational
  producers continue. Future rename is a pure cosmetic follow-up.
- **Out-of-order ToolCallUpdate** — handled explicitly: log and drop, with
  an optional synthesize path when `title` is present in the update.
- **ACP schema evolution adding a new `ToolCallStatus` variant** —
  `ActStatus` is non-`#[non_exhaustive]` today; if ACP adds `Cancelled` or
  similar, the mapping function must be updated in one place. Compiler
  catches this because the match on `ToolCallStatus` is exhaustive.

## Success Criteria

- Spinner on a completed `Act` stops within one tick of receiving the
  terminal `ToolCallUpdate`, regardless of whether that update carries
  `raw_output`.
- Streamed partial `raw_output` during `InProgress` does not stop the
  spinner.
- `entries.len()` grows by exactly 1 per tool call over its full
  lifecycle (one `ToolCall` plus any number of `ToolCallUpdate` events).
- With `observe_collapsed = true`, `render_to_strings` emits one line per
  terminated tool call, carrying the correct outcome glyph derived from
  `ActStatus::Completed` or `ActStatus::Failed`.
- All existing streaming and virtual-row tests pass after the test updates
  described above.
