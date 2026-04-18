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
2. **`ToolCallId` is `agent_client_protocol::ToolCallId`**, a newtype
   `pub struct ToolCallId(pub Arc<str>)` defined in the upstream schema
   crate. `spur_acp` does **not** currently re-export it at its crate root
   (only `ToolCallStatus` is re-exported, `ToolCall` is re-aliased as
   `AcpToolCall` — see `crates/spur-acp/src/lib.rs:46`). This spec adds a
   one-line `pub use agent_client_protocol::ToolCallId;` to
   `spur_acp/src/lib.rs` so TUI code can refer to it as
   `spur_acp::ToolCallId`, keeping the dependency boundary consistent with
   the rest of the crate.
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

At `session_detail.rs:1189` (`SessionUpdate::ToolCall`). This handler must
**preserve** the existing `self.tool_depth.insert(tc.tool_call_id.0.to_string(), depth)`
side-effect at `session_detail.rs:1200` — it drives sub-agent indentation
and is cleared on `TurnComplete` at `session_detail.rs:1345`. The refactor
only changes what is pushed to the trace, not this bookkeeping.

```text
self.tool_depth.insert(tc.tool_call_id.0.to_string(), depth); // unchanged
push Act {
  tool, family, input,
  tool_call_id: Some(tc.tool_call_id.clone()),
  status: map_initial_status(tc.status, tc.raw_output.as_ref(), kind),
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
    if let TraceKind::Act { status, .. } = &mut act_entry.kind:
        *status = merge_status(&*status, tcu.fields.status, tcu.fields.raw_output.as_ref(), kind)
    react_trace.mark_dirty_from(idx)
else:
    tracing::debug!(id = ?tcu.tool_call_id, "ToolCallUpdate for unknown tool_call_id");
    if tcu.fields.title.is_some() || tcu.fields.kind.is_some():
        synthesize a new Act entry from the update
        (status mapped from tcu.fields; tool_call_id = Some(tcu.tool_call_id.clone()))
    else:
        drop
```

Explicit Rust signature for the merge:

```rust
fn merge_status(
    prev: &ActStatus,
    incoming_status: Option<ToolCallStatus>,
    incoming_raw_output: Option<&serde_json::Value>,
    kind: AgentKind,
) -> ActStatus
```

Merge rules:

- If `prev` is already terminal (`Completed(_)` / `Failed(_)`),
  `debug_assert!` that `incoming_status`, when present, matches the terminal
  variant. In release builds log via `tracing::debug!` and return `prev.clone()`
  unchanged. This prevents a late `InProgress` update from reopening a closed
  tool call.
- Else if `incoming_status` is `None`, keep the variant of `prev` and refresh
  `InProgress.partial` only when `prev` is `InProgress` and
  `incoming_raw_output` is `Some(v)`.
- Else map `(incoming_status.unwrap(), incoming_raw_output)` via the table
  above to produce the new variant. An incoming terminal
  (`Completed` / `Failed`) always replaces a non-terminal `prev`.
- Any future `ToolCallStatus` variant not in the table (the enum may become
  `#[non_exhaustive]` upstream) is absorbed at the boundary: log via
  `tracing::debug!` and return `prev.clone()` unchanged. `ActStatus` stays
  exhaustive internally so every renderer is forced to handle every state.

### Renderer change

Spinner-vs-outcome is now a single match on `status`:

```rust
match status {
    ActStatus::Pending | ActStatus::InProgress { .. } => {
        // draw SPINNER_FRAMES[tick]
        // NOTE Phase 1: InProgress.partial is STORED but NOT rendered.
        //      Partial-output streaming is deferred to Phase 2.
    }
    ActStatus::Completed(Some(p)) => {
        // outcome glyph from outcome_glyph(p); body from p
    }
    ActStatus::Completed(None) => {
        // success fallback glyph (✓), no body
    }
    ActStatus::Failed(p_opt) => {
        // fixed failure glyph (⚠ or ✗) — do NOT derive from payload,
        // because a buggy agent could emit Failed with a non-Error payload
        // variant and we'd show a success glyph. Failed ALWAYS renders
        // as failure. Body is rendered from p_opt when Some.
    }
}
```

All three render paths change symmetrically:
`builder.rs:37-78` (markdown-build collapsed Act),
`builder.rs:503-538` (markdown-build expanded Act — reads payload from the
next Observe today; must switch to reading from `status`),
`mod.rs:650-683` (plain-text `render_to_strings` collapsed),
`mod.rs:753-771` (plain-text `render_to_strings` expanded Act input block),
`mod.rs:819-843` (plain-text fallthrough with neighbour-Observe).

The `consumed = 2` bookkeeping disappears — one entry represents one tool
call.

**Expanded-mode (`observe_collapsed == false`) rewrite.** Today, expanded mode
renders the Act header + input block, then the NEXT `Observe` entry renders
its own header + payload block. After the refactor there is no paired
Observe. The expanded renderer must, for a terminal Act, emit the input
block **and** the outcome header + payload block from the same entry. The
output must be byte-equivalent to today's "Act followed by Observe" output
for all terminal states, so that `crates/spur-tui/tests/render_golden.rs`
snapshots change only where the bug-fix intentionally changes the visual.

**Blank-line invariant.** Today `mod.rs:836-843` suppresses the trailing
blank line when an Act is followed by `Observe { payload: Some(_) }`. After
the refactor, each terminal Act emits exactly one trailing blank line in
both collapsed and expanded modes (preserving the current visual spacing
of Act + Observe + blank). In-flight Acts (Pending / InProgress) also emit
one trailing blank line, matching current behaviour.

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
pub(crate) fn find_act_by_id_mut(
    &mut self,
    id: &ToolCallId,
) -> Option<(usize, &mut TraceEntry)>;
```

Scans `self.entries` in reverse — O(N), bounded by `MAX_LOG_ENTRIES = 500`.
Returns the newest matching `Act`. Match is on the inner `Arc<str>`:

```rust
for (idx, e) in self.entries.iter_mut().enumerate().rev() {
    if let TraceKind::Act { tool_call_id: Some(existing), .. } = &e.kind {
        if existing.0.as_ref() == id.0.as_ref() {
            return Some((idx, e));
        }
    }
}
None
```

The `.0.as_ref()` comparison bypasses any `PartialEq` surprises on the
`ToolCallId` newtype — we're comparing string content, not Arc identity.
Caller mutates `status` and invokes `self.mark_dirty_from(idx)`. Mirrors
`attach_executor_id`'s pattern at `mod.rs:615-635`.

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

Existing tests that construct `TraceKind::Act { ... }` literals get updated
with the new fields. The complete blast radius across the repo is 14 match
sites (verified by grep) plus uncommitted benchmark scaffolding:

| File | Line(s) | Nature |
|------|---------|--------|
| `crates/spur-tui/src/views/session_detail.rs` | 1215 | Production push site — rewritten to use `ActStatus`. |
| `crates/spur-tui/src/components/react_trace/builder.rs` | 38, 149, 323, 487, 503, 666, 912 | Renderer match arms — collapsed + expanded paths rewritten. |
| `crates/spur-tui/src/components/react_trace/mod.rs` | 174, 472, 655, 753, 836, 1195, 1407, 1435 | Kind-name helper, tick animator, renderers, tests. |
| `crates/spur-tui/tests/render_golden.rs` | 26 | Golden snapshot — regenerate after renderer rewrite once manual visual review confirms equivalence for terminal states. |
| `crates/spur-tui/benches/*` (uncommitted per `git status`) | — | Check for `Act` constructions; update to new shape. |
| `crates/spur-tui/examples/react_trace_bench_sim.rs` (uncommitted) | — | Same as above. |

**Test-helper migration default.** Every existing `TraceKind::Act { ... }`
construction adds `tool_call_id: None, status: ActStatus::Pending` unless the
test explicitly exercises a terminal state, in which case
`status: ActStatus::Completed(Some(payload))` or
`ActStatus::Failed(Some(payload))` replaces the neighbour `Observe` entry
the test previously pushed.

**Specific test transformation to call out.** The test
`entry_row_starts_remain_indexed_by_absolute_entry_after_collapsed_pairs` at
`mod.rs:1193-1232` pushes `Act` then `Observe{Some(Text)}` to verify that
the `entry_row_starts` vector stays aligned with absolute entry indices
across the collapsed-pair render. After the refactor there is ONE entry
(`Act { status: Completed(Some(Text)) }`) instead of two; the test still
verifies the alignment invariant but reads more cleanly because the
"pair" goes away entirely. The invariant itself is reinforced, not
weakened.

## Risks and Mitigations

- **Test-surface churn** — bounded: 14 match sites across 4 files plus the
  uncommitted benches/examples. Golden snapshot requires regeneration with
  manual visual review for terminal states.
- **`TraceKind::Observe` dual purpose during the transition** — tolerable.
  Tool-call producers stop pushing Observe on Day 1; informational
  producers continue. Future rename is a pure cosmetic follow-up.
- **Out-of-order ToolCallUpdate** — handled explicitly: log and drop, with
  an optional synthesize path when `title` or `kind` is present in the
  update.
- **ACP schema evolution adding a new `ToolCallStatus` variant** — absorbed
  at the boundary: unknown variants leave `prev` unchanged and emit a
  `tracing::debug!` message. `ActStatus` stays exhaustive internally so
  every renderer is forced by the compiler to handle every state.
- **`ObservePayload` Clone cost under streaming updates** —
  `ObservePayload::CommandOutput` can carry large `stdout`/`stderr`
  `String`s. Each `merge_status` call that replaces `InProgress.partial`
  clones the new payload in. Under high-frequency streaming (e.g. 10 Hz
  bash output) this is O(N·bytes) churn. Acceptable for v1 since Phase 1
  does not RENDER partial and therefore does not stress this path. A
  zero-copy ring-buffer for streamed partial output is a Phase 2
  optimization if Phase 2 lands.
- **Upstream crate boundary** — adding
  `pub use agent_client_protocol::ToolCallId;` to
  `crates/spur-acp/src/lib.rs` is a semver-minor re-export. No behavioural
  change, just makes the type reachable as `spur_acp::ToolCallId`.

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
