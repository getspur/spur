# ACP SessionEvent Pass-Through (Sub-project 1 of 4)

## Problem

`notification_to_session_event()` in `orchestrator.rs` converts the ACP SDK's rich `SessionUpdate` (10 variants, nested structs with tool status, tool kind, plan entries, file locations) into spur's simplified `SessionEvent` (7 variants, flat strings). This destroys information:

- Tool status lifecycle (Pending → InProgress → Completed → Failed) — dropped
- Tool kind (Read/Edit/Execute/Search) — dropped
- Plan entries with statuses — dropped entirely
- File locations affected by tool calls — dropped
- Non-text content blocks (image, audio, resource) — dropped

The TUI cannot show tool progress, plan visualization, or kind-based icons.

## Solution

Delete `SessionEvent`. Pass the SDK's `SessionNotification` through `SpurEvent` verbatim. The TUI matches on `SessionUpdate` variants directly.

### Why pass-through instead of enriching SessionEvent

`SessionEvent` is a redundant intermediate type. spur-acp already depends on `agent-client-protocol`. The transitive dependency is already real — `SessionEvent` adds maintenance burden without adding isolation:

- Every time the SDK adds useful data to existing variants, `SessionEvent` and the mapping function must be manually updated. With pass-through, new data is available automatically.
- Every new `SessionUpdate` variant falls through the catch-all and is invisible to spur. With pass-through, the TUI can handle it whenever convenient.
- The spur-specific `SessionEvent` variants (`Complete`, `Error`, `RateLimitHit`) are already redundant with existing `SpurEvent` variants (`SessionCompleted`, `BrainError`, `RateLimitDetected`).

## Design

### SpurEvent change

```rust
// Before:
SpurEvent::AgentOutput { session: SessionId, event: SessionEvent }

// After:
SpurEvent::AgentNotification { session: SessionId, notification: SessionNotification }
```

`SessionNotification` is the SDK type containing `{ session_id: SessionId, update: SessionUpdate }`.

### Re-exports from spur-acp

Add to `spur-acp/src/lib.rs`:

```rust
pub use agent_client_protocol::{
    SessionNotification, SessionUpdate,
    ContentBlock, ContentChunk, TextContent,
    ToolCall, ToolCallUpdate, ToolCallUpdateFields,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
};
```

This gives the TUI full access without adding `agent-client-protocol` to its own `Cargo.toml`.

### Deleted code

1. `SessionEvent` enum from `spur-acp/src/types.rs` — all 7 variants removed.
2. `notification_to_session_event()` from `spur-core/src/orchestrator.rs` — the entire mapping function (~45 lines) plus its helper functions.

### Orchestrator changes

The orchestrator currently:
1. Calls `notification_to_session_event(notification)` to convert
2. Wraps in `SpurEvent::AgentOutput { session, event }`
3. Broadcasts
4. Internally matches on `SessionEvent` for rate-limit detection, completion, text accumulation

After:
1. Wraps in `SpurEvent::AgentNotification { session, notification }` — no conversion
2. Broadcasts
3. Internally matches on `notification.update` (SessionUpdate variants) directly

### TUI changes

**session_detail.rs** — the main consumer. Current match on `SessionEvent` becomes match on `SessionUpdate`:

```rust
match &notification.update {
    SessionUpdate::AgentThoughtChunk(chunk) => {
        if let Some(text) = extract_text(chunk) {
            self.react_trace.append_think(text, timestamp);
        }
    }
    SessionUpdate::AgentMessageChunk(chunk) => {
        if let Some(text) = extract_text(chunk) {
            self.react_trace.append_message(text, &self.agent_name, timestamp);
        }
    }
    SessionUpdate::ToolCall(tc) => {
        // Full access: tc.title, tc.kind, tc.status, tc.raw_input, tc.locations
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: tc.title.clone(),
                args: format_tool_args(&tc.raw_input.clone().unwrap_or_default()),
            },
            text: String::new(),
            timestamp,
        });
    }
    SessionUpdate::ToolCallUpdate(tcu) => {
        // Full access: tcu.fields.status, tcu.fields.raw_output, tcu.fields.locations
        let output = tcu.fields.raw_output.clone().unwrap_or(serde_json::Value::Null);
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Observe,
            text: format_observe_output(&output),
            timestamp,
        });
    }
    SessionUpdate::Plan(plan) => {
        // NEW: Plan entries are now visible
        // Render as a checklist in the trace (implementation can be minimal)
    }
    _ => {} // Future variants handled gracefully
}
```

**Helper function** added to session_detail.rs:
```rust
fn extract_text(chunk: &ContentChunk) -> Option<&str> {
    match &chunk.content {
        ContentBlock::Text(tc) => Some(&tc.text),
        _ => None,
    }
}
```

**dashboard.rs** — same pattern. Update `SessionEvent` match arms to `SessionUpdate`.

### TUI rendering improvements (enabled by pass-through)

With full SDK data available, the TUI can now display:

1. **Tool status indicators** — `tc.status` / `tcu.fields.status`:
   - `Pending` → dim spinner
   - `InProgress` → active spinner
   - `Completed` → green checkmark
   - `Failed` → red X

2. **Tool kind icons** — `tc.kind`:
   - `Read` → file icon
   - `Edit` → pencil icon
   - `Execute` → terminal icon
   - `Search` → magnifying glass
   - Others → default wrench

3. **Plan entries** — `plan.entries`:
   - Render as a checklist with `[x]` / `[ ]` / `[~]` indicators

4. **File locations** — `tc.locations`:
   - Show affected file paths below tool title

These rendering improvements are optional in this sub-project — the data is available, rendering can be enhanced incrementally.

## Files changed

| File | Change |
|------|--------|
| `spur-acp/src/types.rs` | Delete `SessionEvent` enum. Delete `AgentStatus` enum (only used by `SessionEvent::StatusUpdate`, no other consumers). |
| `spur-acp/src/lib.rs` | Add SDK re-exports |
| `spur-acp/src/domain/events.rs` | Rename `AgentOutput` → `AgentNotification`, carry `SessionNotification` |
| `spur-core/src/orchestrator.rs` | Delete `notification_to_session_event()` and helpers. Emit `AgentNotification` directly. Update internal match arms on `SessionUpdate`. |
| `spur-tui/src/views/session_detail.rs` | Match on `SessionUpdate` variants. Add `extract_text()` helper. |
| `spur-tui/src/views/dashboard.rs` | Update `SessionEvent` match arms → `SessionUpdate`. |

## What does NOT change

- `SpurEvent` variants other than `AgentOutput` (BrainSpawned, TurnComplete, etc.)
- TUI components (ReactTrace, ActivityLog, InputBar)
- AgentConnection trait or any connection implementation
- Permission handling (Sub-project 2)
- Terminal operations (Sub-project 3)
- Session management (Sub-project 4)

## Migration safety

All match arm changes are compiler-enforced. Renaming `SessionEvent::ToolCallStart` → `SessionUpdate::ToolCall` produces compile errors at every call site. No silent breakage.

## Dependency impact

No new crate dependencies. `agent-client-protocol` is already a transitive dependency of `spur-tui` via `spur-acp`. The re-exports make the types explicitly available without adding it to `spur-tui/Cargo.toml`.
