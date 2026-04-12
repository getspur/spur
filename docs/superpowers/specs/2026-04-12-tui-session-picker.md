# TUI Session Picker

## Problem

When `spur watch` starts, there's no way to browse or resume previous agent sessions from the TUI. The `--resume <id>` flag (sub-project 4) requires knowing the session ID upfront. Users who want to continue a prior conversation must exit spur, look up session IDs from the agent's storage, and re-launch with the flag.

## Solution

Add a `SessionPickerView` to the TUI that lists resumable sessions from the ACP agent via `list_sessions`. The picker is triggered on-demand via a `[s]` keybinding on the Dashboard, or automatically on launch with `--sessions`. Selecting a session calls `load_session` and streams history into the existing `SessionDetailView`.

## Dependency

Requires sub-project 4 (ACP Session Management) — specifically `list_sessions` and `load_session` on `AgentConnection`, and the `SessionInfo` re-export from `spur-acp`.

**Note on orchestrator design:** This spec's `connect_brain` / `create_brain_session` / `load_brain_session` decomposition supersedes sub-project 4's proposed `resume_session_id` parameter on `spawn_brain_session`. Sub-project 4 should implement the decomposition directly (it serves both `--resume` and the picker). The picker then reuses these methods rather than refactoring them.

## Scope (v1)

**Implement:**
- `SessionPickerView` implementing `View` trait (loading, populated, empty, error states)
- `[s]` keybinding on Dashboard (when no active session)
- `--sessions` CLI flag on `spur watch`
- Adaptive compact row layout based on actual `SessionInfo` fields
- `InteractiveInput` and `UserInput` enums (replace structs)
- `SpurEvent::SessionsListed` and `SpurEvent::SessionsListError`
- Orchestrator `connect_brain` refactor (decompose `spawn_brain_session`)
- Orchestrator handlers for `ListSessions` and `ResumeSession`

**Defer (v2):**
- Mid-session picker (opening picker from SessionDetailView while a session is active)
- Session filtering/search (`/` to type filter)
- Session deletion from TUI
- Session metadata preview panel
- Cached sessions for instant launch display
- Multiple agent session aggregation
- Spur-local enrichment (cross-reference CostTracker for turns/cost)

## Design

### Trigger: on-demand with `--sessions` sugar

The picker does NOT show on every launch. It is triggered by:

1. **`[s]` keybinding** on Dashboard — user-initiated, triggers agent spawn + `list_sessions`
2. **`--sessions` CLI flag** — TUI starts in picker view and auto-sends `ListSessions` on init
3. **`--resume <id>`** — bypasses picker entirely (sub-project 4, not this spec)

If both `--sessions` and `--resume` are passed, `--resume` takes priority.

The Dashboard splash screen adds a hint: "Press [s] to resume a session".

**Rationale:** `list_sessions` requires a live agent connection (1-3s cold-start for process spawn + ACP initialize). Showing the picker on every launch penalizes users who want a fresh session. The on-demand approach preserves zero-overhead startup for the common case. The `--sessions` flag is syntactic sugar — it auto-triggers the same flow as pressing `[s]`.

### Layout: adaptive compact (one row per session)

`SessionInfo` from the ACP SDK provides: `session_id`, `title` (optional), `updated_at` (optional), `cwd`, `meta` (optional). The layout adapts to available data:

**Row format:**
```
{8-char-id} · {display_text}                    {relative_time}
```

**Display text resolution:**
1. If `title` is `Some`: use the title
2. If `title` is `None` and sessions span multiple cwds: use `cwd` basename (e.g., `spur/`)
3. If `title` is `None` and all sessions share the same cwd: show `(untitled session)`

**Cwd suffix:** When sessions have heterogeneous `cwd` values, a cwd basename suffix appears on ALL rows (right-aligned before the time). When all cwds match, no suffix is shown.

**Examples:**

Common case (titles present, same cwd):
```
Sessions (kiro-cli)

▸ a3f8c1d2 · Add TUI session picker feature          2h ago
  7b2e9f01 · Fix rate limit handling in orchestrator   5h ago
  e5d4a390 · Implement permission flow for ACP         1d ago
  1c7f8b44 · Debug worktree conflict detection         2d ago

  ↑↓ navigate · Enter select · Esc back
```

Edge case (mixed titles, multiple cwds):
```
Sessions (kiro-cli)

▸ a3f8c1d2 · Add TUI session picker          spur/    2h ago
  7b2e9f01 · Fix rate limit handling          spur/    5h ago
  e5d4a390 · (untitled session)               webapp/  1d ago

  ↑↓ navigate · Enter select · Esc back
```

**Session count:** v1 shows all sessions returned by `list_sessions` in a scrollable list without pagination or limits. Agents typically return 10-50 sessions. For very long lists, the user can scroll or use `--resume <id>` directly. Filtering/search is deferred to v2.

**Rationale:** The ACP `SessionInfo` struct has only 5 fields. Richer layouts (two-line rows, split panels) were evaluated but rejected — they display data that doesn't exist in the API (turns, cost, last message). The adaptive compact layout maximizes visible sessions (~12-15 in a 24-row terminal) while gracefully handling missing titles.

### View states

`SessionPickerView` has four states:

**Loading** — shown immediately when user presses `[s]` or on `--sessions` launch:
```
Sessions

  Connecting to kiro-cli ···

  Esc cancel
```

**Populated** — after `SessionsListed` event arrives with non-empty list. Scrollable, arrow key navigation, Enter to select. When user presses Enter, the selected row shows a "loading..." indicator while the orchestrator processes `load_session`. The view stays in populated state (not a separate state) — the indicator is just visual feedback on the selected row.

**Empty** — after `SessionsListed` arrives with zero sessions:
```
Sessions (kiro-cli)

  No saved sessions found.
  Start a new conversation from the dashboard.

  Esc back
```

**Error** — after `SessionsListError` arrives:
```
Sessions

  Session listing not supported by kiro-cli.
  Use --resume <id> to load a session by ID.

  Esc back
```

### Data flow

```
[s] or --sessions
  → TUI: App transitions to SessionPicker (loading state)
  → TUI: sends UserInput::ListSessions
  → main.rs: forwarding task maps to InteractiveInput::ListSessions
  → Orchestrator: connect_brain() if no connection, then list_sessions()
  → Orchestrator: emits SpurEvent::SessionsListed { agent, sessions }
  → TUI: SessionPicker renders populated/empty list

User presses Enter on a session
  → TUI: SessionPicker enters "resuming" state
  → TUI: sends UserInput::ResumeSession { session_id }
  → main.rs: forwarding task maps to InteractiveInput::ResumeSession
  → Orchestrator: load_brain_session(connection, session_id)
  → Orchestrator: emits BrainSpawned
  → TUI: auto-navigates from SessionPicker to SessionDetail
  → Orchestrator: drains history stream as AgentNotification events
  → TUI: SessionDetail ReactTrace renders history in real-time
  → Orchestrator: emits TurnComplete → user can type new messages

User presses Esc during loading
  → TUI: navigates back to Dashboard
  → Orchestrator: completes ListSessions, emits SessionsListed
  → TUI: ignores event (not on picker view)
  → Orchestrator: agent_connection stays alive for reuse

Error during list_sessions or load_session
  → Orchestrator: emits SessionsListError or BrainError
  → TUI: shows error state, user presses Esc to go back
```

### Channel design: enum-based InteractiveInput

The TUI-to-orchestrator channel uses a single `mpsc` with enum variants:

```rust
// spur-core
pub enum InteractiveInput {
    Message { text: String, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
}

// spur-tui
pub enum UserInput {
    Message { session: SessionId, text: String, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
}
```

The forwarding task in `main.rs` maps `UserInput` variants to `InteractiveInput` variants 1:1. Single channel preserves message ordering. Session commands in v1 only arrive from Dashboard (no active streaming), so they never contend with the `pending_messages` queue.

**Rationale:** A separate control channel was evaluated but rejected — it doubles the wiring (7+ touch points vs 5), introduces cross-channel ordering ambiguity, and adds `select!{}` branches to both the outer and inner orchestrator loops. The priority benefit of separate channels is theoretical in v1 (session commands don't arrive during streaming).

### Orchestrator connection lifecycle refactor

`spawn_brain_session` is decomposed into three methods:

```rust
/// Phase 1: Resolve agent, create connection, initialize.
/// Returns a connected-but-no-session connection.
async fn connect_brain(&mut self, brain_override: Option<&str>)
    -> Result<Box<dyn AgentConnection>>

/// Phase 2a: Start MCP server, create new session, start delegation handler.
async fn create_brain_session(&mut self, connection: Box<dyn AgentConnection>, ...)
    -> Result<BrainSession>

/// Phase 2b: Start MCP server, load existing session, start delegation handler.
/// Returns the BrainSession and the history notification stream.
async fn load_brain_session(&mut self, connection: Box<dyn AgentConnection>, session_id: &str, ...)
    -> Result<(BrainSession, Pin<Box<dyn Stream<Item = SessionNotification> + Send>>)>
```

`run_interactive` gains a local `agent_connection: Option<Box<dyn AgentConnection>>` that holds the initialized-but-no-session connection between `ListSessions` and `ResumeSession`/`Message`.

- `ListSessions`: calls `connect_brain()` if `agent_connection` is `None`, then `list_sessions()`
- `ResumeSession`: takes `agent_connection`, calls `load_brain_session()`
- `Message` (existing): takes `agent_connection` or calls `connect_brain()`, then `create_brain_session()`

`LoadSessionRequest` accepts `mcp_servers` (verified from ACP SDK v0.10.4), so delegation works in resumed sessions.

**Rationale:** `spawn_brain_session` does `initialize + new_session`. For `list_sessions`, only `initialize` is needed. For `load_session`, `new_session` is wrong (creates a conflicting session). The decomposition serves both the picker and the `--resume` flag from sub-project 4.

### `--sessions` flag integration

Pass `start_in_picker: bool` to `run_tui`. When true, `App::new` starts with `current_view = ViewId::SessionPicker` and immediately sends `UserInput::ListSessions` through `user_input_tx`. Same data flow as pressing `[s]`, just auto-triggered at startup.

## New SpurEvent variants

```rust
pub enum SpurEvent {
    // ... existing variants ...
    SessionsListed { agent: String, sessions: Vec<SessionInfo> },
    SessionsListError { message: String },
}
```

`agent` in `SessionsListed` allows the picker header to show "Sessions (kiro-cli)".

## Files changed

| File | Change |
|------|--------|
| `spur-acp/src/domain/events.rs` | 2 new SpurEvent variants: `SessionsListed`, `SessionsListError` |
| `spur-core/src/orchestrator.rs` | `InteractiveInput` struct → enum. Decompose `spawn_brain_session` into `connect_brain`, `create_brain_session`, `load_brain_session`. Add `agent_connection` local state to `run_interactive`. Add `ListSessions` and `ResumeSession` match arms in outer loop. |
| `spur-tui/src/action.rs` | Add `ViewId::SessionPicker`. Add `Action::RequestSessions` and `Action::ResumeSession { session_id: String }`. |
| `spur-tui/src/views/session_picker.rs` | **New file.** `SessionPickerView` implementing `View` trait. Four states (loading, populated, empty, error). Adaptive compact row rendering. Arrow key navigation, Enter select, Esc back. ~100 lines. |
| `spur-tui/src/views/mod.rs` | Add `pub mod session_picker;` |
| `spur-tui/src/views/dashboard.rs` | Add `'s'` to single-char nav key match → `Action::RequestSessions`. Add `_ => {}` catchall to `handle_spur_event` for new SpurEvent variants. |
| `spur-tui/src/components/status_bar.rs` | Add `ViewId::SessionPicker` arm to hints match: `" [↑↓]navigate [Enter]select [Esc]back"`. |
| `spur-tui/src/app.rs` | `UserInput` struct → enum. Add `session_picker: Option<SessionPickerView>` field. Handle `SessionsListed`/`SessionsListError` in `handle_spur_event`. Extend `BrainSpawned` auto-navigate to include `ViewId::SessionPicker`. Handle `Action::RequestSessions` and `Action::ResumeSession` in `process_action`. Add `start_in_picker` parameter to `run_tui`. Render/key/tick routing for `ViewId::SessionPicker`. |
| `spur-cli/src/main.rs` | Add `--sessions` flag to `Watch` command. Pass `start_in_picker` to `run_tui`. Update forwarding task to map new `UserInput` variants to `InteractiveInput` variants. |

## What does NOT change

- `AgentConnection` trait (consumes `list_sessions`/`load_session` from sub-project 4)
- `NativeAcpConnection` (already implemented in sub-project 4)
- `StdioAdapter` / `CliWrapAdapter` (inherit default error implementations)
- `SessionDetailView` (history renders automatically via existing `AgentNotification` path)
- `ReactTrace`, `InputBar` components
- `session_detail.rs` `handle_spur_event` (already has `_ => {}` catchall)
- Permission flow, delegation, cost tracking
