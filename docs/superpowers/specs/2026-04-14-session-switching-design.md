# Session Switching — Design

Date: 2026-04-14
Status: Approved for implementation

## Problem

A user inside an active `SessionDetailView` has no direct way to switch to
another session. The only path is `Esc` → Dashboard → `s` → picker →
pick row. Two navigation hops, and `s` requires an empty input bar, so a
half-written draft blocks the shortcut.

Beyond the UX gap, the switch itself exposes correctness bugs:
- The orchestrator's `ResumeSession` arm overwrites the active
  `BrainSession` without teardown — leaking the prior delegation
  handle, connection, and MCP server.
- `Action::NewSessionRequested` in `app.rs:707` is a stub that navigates
  to Dashboard without asking the orchestrator to shut down the current
  brain, so the old agent subprocess keeps running.
- The picker will re-resume the currently-active session if the user
  picks its row — wasteful and surprising.

## Goal

From inside an active session, provide a fast, predictable path to
switch to another session or start a new one, without leaking the
prior brain and without re-loading the same session the user is in.

## Model

- **SessionDetail entry points** — two equivalent triggers:
  `Alt+s` (keyboard shortcut, state-agnostic) and `/sessions`
  (slash command, discoverable via `/` completion).
- **Mid-turn behavior** — if the brain is streaming, `ResumeSession`
  is *queued* by `run_interactive` (via the inner `tokio::select!` in
  the Message arm) and fires after the turn completes. Users who want
  to abandon mid-turn must interrupt first (`!<enter>`). This matches
  the existing interrupt idiom and avoids an ad-hoc cancel variant.
- **Single-brain invariant** — at most one active `BrainSession` at
  any time. Switching == teardown + load.

## Design

### 1. Entry points from `SessionDetailView`

In `crates/spur-tui/src/views/session_detail.rs`
`handle_key_inner`, add an early match arm — adjacent to the
existing `Alt+m` (mode) and `Alt+v` (mermaid) handlers:

```rust
if matches!(key.code, KeyCode::Char('s'))
    && key.modifiers.contains(KeyModifiers::ALT)
{
    return Some(Action::RequestSessions);
}
```

In `crates/spur-tui/src/commands/spur_local.rs`, append a
`CommandEntry`:

```rust
CommandEntry {
    name: "sessions".into(),
    description: "Open session picker".into(),
    hint: None,
    source: CommandSource::Spur,
    dispatch: Dispatch::SpurLocal(Action::RequestSessions),
},
```

`Action::RequestSessions` already (a) flushes the active draft via
`force_flush_active_draft`, (b) refreshes metadata, (c) opens the
picker, (d) requests `ListSessions`. No app.rs change needed for the
entry point.

### 2. Picker: short-circuit on current-session pick

In `crates/spur-tui/src/views/session_picker.rs`:

- Add `current_session_id: Option<String>` field on `SessionPickerView`
  alongside the existing `current_session_with_draft`.
- Add setter `set_current_session_id(&mut self, sid: Option<String>)`.
- In the Enter handler (around line 885, where a non-zero cursor
  currently dispatches `ResumeSession`), short-circuit before the
  draft-confirm check:

```rust
if current_session_id.as_deref() == Some(&sid) {
    Some(Action::NavigateTo(ViewId::SessionDetail(SessionId(sid))))
} else {
    // existing draft-confirm + ResumeSession path
}
```

In `crates/spur-tui/src/app.rs` `refresh_picker_metadata`, push the
current session id alongside the draft awareness:

```rust
fn refresh_picker_metadata(&mut self) {
    let draft = self.compute_draft_session();
    let current = self.session_detail.as_ref().map(|d| d.session_id().0.clone());
    if let Some(ref mut picker) = self.session_picker {
        picker.set_metadata(self.metadata_store.metadata().clone());
        picker.set_current_session_has_draft(draft);
        picker.set_current_session_id(current);
    }
}
```

### 3. Fix `Action::NewSessionRequested` stub

Replace the stub in `crates/spur-tui/src/app.rs:707`:

```rust
Action::NewSessionRequested => {
    // Shut down the current brain atomically so picker [+ New session]
    // doesn't leave the old agent subprocess running.  The orchestrator's
    // NewSessionWithMessage arm with empty blocks is defined as
    // "teardown + defer spawn to next Message" (orchestrator.rs:748-761).
    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::NewSessionWithMessage {
            blocks: vec![],
            interrupt: false,
        });
    }
    self.current_view = ViewId::Dashboard;
    self.dirty = true;
}
```

### 4. Fix orchestrator `ResumeSession` leak

In `crates/spur-core/src/orchestrator.rs`, at the top of the
`InteractiveInput::ResumeSession` arm (around line 447), before the
`agent_connection.take()` / `connect_brain` branch:

```rust
// If there's an active brain, tear it down before loading the new one.
// Mirrors the NewSessionWithMessage cleanup block.
if let Some(mut b) = brain.take() {
    b.delegation_handle.abort();
    let _ = b.connection.shutdown().await;
    let _ = b.mcp_server.shutdown();
}
```

`connection.cancel()` is NOT needed here because `ResumeSession` is
queued while a turn is streaming (see "Mid-turn behavior" above) — by
the time this arm runs, the stream has already completed.

### 5. Explicit non-scope

- **Cancel-and-switch shortcut.** Mid-turn interrupts require the
  user to type `!<enter>` first. Revisit if users complain.
- **Multi-brain / tab-style peek.** The single-brain invariant is
  load-bearing for the orchestrator's current design; parallel brains
  would require re-architecting `run_interactive`, the agent_connection
  slot, and event routing. Not v1.
- **`resumed=false` toast after user-initiated resume.** Would need
  to distinguish user-intent-resume from fresh spawn at the event
  level. Defer; silent recovery is acceptable.
- **Pseudo-row "+ New session"** — already exists in the picker.
- **Draft-confirm switch banner** — already exists in the picker.
- **`n` keybinding for new session in picker** — already exists.

## Tests

1. **Spur-local command resolves** (`crates/spur-tui/src/commands/`):
   `submit_router::route("/sessions", ...)` returns
   `SubmitDecision::Local { action: Action::RequestSessions }`.

2. **Picker short-circuits current session**
   (`crates/spur-tui/src/views/session_picker.rs` unit):
   Given a picker with `current_session_id = Some("X")` and cursor on
   the row for "X", Enter returns
   `Action::NavigateTo(ViewId::SessionDetail(SessionId("X")))` —
   not `Action::ResumeSession`.

3. **NewSessionRequested dispatches teardown**
   (integration): Feeding `Action::NewSessionRequested` into the App
   results in a `UserInput::NewSessionWithMessage { blocks: [],
   interrupt: false }` being sent on the user_input channel.

4. **Orchestrator shuts down prior brain on Resume** (spur-core):
   With a mocked `BrainSession` wired into `run_interactive` and then
   a `ResumeSession` dispatched, assert `connection.shutdown` and
   `mcp_server.shutdown` are called on the prior brain before the
   new one is loaded. (Requires a test mock; if the harness is too
   heavy, demote this to a manual-smoke verification.)

5. **Manual smoke:** with two existing claude-code-acp sessions in
   `session_metadata.json`, from an active SessionDetail on A, press
   `Alt+s`, select B, observe:
   - Previous agent subprocess terminates cleanly (`ps aux | grep node`
     shows at most one claude-code-acp process at any time).
   - B's history renders.
   - Pressing `Alt+s` again and selecting A's row navigates back to A
     without re-loading (no new BrainSpawned event for A).

## Files

- `crates/spur-tui/src/commands/spur_local.rs` — +1 `CommandEntry`.
- `crates/spur-tui/src/views/session_detail.rs` — +Alt+s early match.
- `crates/spur-tui/src/views/session_picker.rs` —
  `current_session_id` field + setter + short-circuit in Enter handler.
- `crates/spur-tui/src/app.rs` — fix `NewSessionRequested` handler;
  push current_session_id in `refresh_picker_metadata`.
- `crates/spur-core/src/orchestrator.rs` — cleanup block in
  `ResumeSession` arm.

Expected size: ~60-80 LoC across 5 files.
