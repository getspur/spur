# Session Switching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user inside an active SessionDetail switch to another session (or start a new one) via `Alt+s` / `/sessions` without leaking the prior brain, without restarting the agent subprocess, and without pointlessly re-resuming the session they're already in.

**Architecture:** Three narrow fixes on top of existing picker machinery: (1) add entry points from SessionDetail; (2) teach the picker about the current session id so Enter on the current row short-circuits to NavigateTo; (3) consolidate orchestrator brain teardown into a `retire_active_brain` helper that preserves the initialized ACP connection across `ResumeSession` and `NewSessionWithMessage`, plus fix the `NewSessionRequested` stub in app.rs so it actually shuts the current brain down.

**Tech Stack:** Rust, tokio, ratatui, crossterm, tracing.

---

## File Structure

- `crates/spur-tui/src/commands/spur_local.rs` — +1 `CommandEntry` for `/sessions`.
- `crates/spur-tui/src/views/session_detail.rs` — `Alt+s` early match in `handle_key_inner`.
- `crates/spur-tui/src/views/session_picker.rs` — `current_session_id` field + setter + short-circuit branch in Enter handler.
- `crates/spur-tui/src/app.rs` — push `current_session_id` in `refresh_picker_metadata`; replace `NewSessionRequested` stub with a real handler.
- `crates/spur-core/src/orchestrator.rs` — extract `retire_active_brain(&mut brain, &mut agent_connection)` helper; call it from both `ResumeSession` and `NewSessionWithMessage` arms instead of inline full-shutdown blocks.

Expected size: ~80-100 LoC total.

---

## Task 1: Add `/sessions` slash command

**Files:**
- Modify: `crates/spur-tui/src/commands/spur_local.rs`

- [ ] **Step 1: Verify clean working tree**

```bash
git diff --stat HEAD -- '*.rs'
# expected: empty
git rev-parse --short HEAD
# record the SHA for your first commit's parent
```

If the tree is dirty, STOP and ask the controller before proceeding.

- [ ] **Step 2: Append the entry**

In `crates/spur-tui/src/commands/spur_local.rs`, inside `SpurLocalSource::entries()`, add a new `CommandEntry` after `mode` and before `cost` (alphabetical-ish):

```rust
CommandEntry {
    name: "sessions".into(),
    description: "Open session picker".into(),
    hint: None,
    source: CommandSource::Spur,
    dispatch: Dispatch::SpurLocal(Action::RequestSessions),
},
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: compiles clean.

- [ ] **Step 4: Write a test asserting `/sessions` routes to `RequestSessions`**

Check whether `crates/spur-tui/src/commands/submit_router.rs` or a sibling module has existing unit tests. If it does, append there; otherwise create `crates/spur-tui/src/commands/submit_router.rs` inline `#[cfg(test)] mod tests` at the bottom. Test body:

```rust
#[cfg(test)]
mod sessions_slash_tests {
    use super::*;
    use crate::commands::registry::CommandRegistry;

    #[test]
    fn slash_sessions_routes_to_request_sessions() {
        let registry = CommandRegistry::default();  // picks up spur_local entries
        let decision = route("/sessions", &[], &registry, false);
        match decision {
            SubmitDecision::Local { action: Action::RequestSessions } => {}
            other => panic!("expected Local { action: RequestSessions }, got {:?}", other),
        }
    }
}
```

If `CommandRegistry::default()` doesn't include spur_local entries, use the constructor that does — inspect `crates/spur-tui/src/commands/registry.rs` to see how `CommandRegistry` is typically built (likely via `CommandRegistry::from_sources(...)` or similar). Use that constructor with `SpurLocalSource::entries()`.

- [ ] **Step 5: Run test**

Run: `cargo test -p spur-tui sessions_slash -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/commands/spur_local.rs crates/spur-tui/src/commands/submit_router.rs
git commit -m "feat(spur-tui): add /sessions slash command"
```

---

## Task 2: Add `Alt+s` shortcut in SessionDetail

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Locate the Alt-key cluster**

Grep for `Alt+m` or `KeyModifiers::ALT` in `crates/spur-tui/src/views/session_detail.rs`. The existing handler is around line 561:

```rust
if matches!(key.code, KeyCode::Char('m')) && key.modifiers.contains(KeyModifiers::ALT) {
    return Some(Action::TogglePlanMode);
}
```

The pattern is: Alt-key checks happen BEFORE the permission-pending check and BEFORE the popup check.

- [ ] **Step 2: Add `Alt+s` adjacent to `Alt+m`**

Immediately after the `Alt+m` block (and before the Alt+v / permission checks), add:

```rust
// Alt+s → open session picker. Mirrored by the /sessions slash command.
// Matched early so it works even while the input bar has focus or a
// permission prompt is pending (user can bail out of a session at any
// time; the orchestrator auto-denies the pending permission when the
// brain is torn down).
if matches!(key.code, KeyCode::Char('s')) && key.modifiers.contains(KeyModifiers::ALT) {
    return Some(Action::RequestSessions);
}
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): Alt+s opens session picker from SessionDetail"
```

(No unit test — handle_key_inner is not unit-tested today and the keybinding is trivially verified via the manual smoke in Task 7. If an existing test harness is found, add a test there; otherwise proceed.)

---

## Task 3: Add `current_session_id` to `SessionPickerView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Add field, initializer, and setter**

Inside `pub struct SessionPickerView` (around line 64–84), add a new field alongside `current_session_with_draft`:

```rust
/// SPUR session id of the currently-active SessionDetail, if any.
/// Distinct from `current_session_with_draft` (which is Some only when
/// the active session has UNSENT draft text). Used so that Enter on the
/// current session's row short-circuits to NavigateTo instead of
/// pointlessly re-resuming the session the user is already in.
current_session_id: Option<String>,
```

In `SessionPickerView::new()` (around line 87–98), initialize:

```rust
current_session_id: None,
```

Add the setter method adjacent to `set_current_session_has_draft`:

```rust
/// Push the SPUR session id of the currently-active SessionDetail into
/// the picker. `None` when Dashboard is the active view.
pub fn set_current_session_id(&mut self, session_id: Option<String>) {
    self.current_session_id = session_id;
}
```

- [ ] **Step 2: Short-circuit Enter on the current row**

In `handle_key` near line 886, the existing Enter branch for `*cursor > 0` destructures an entry from `filtered_indices`, picks up `sid`, and dispatches either `StartConfirmSwitch` (if draft elsewhere) or `ResumeSession`. We add a BEFORE-draft-check short-circuit.

First, extend the split-borrow destructure at line 805–811 to include the new field:

```rust
let SessionPickerView {
    state,
    metadata,
    show_archived,
    current_session_with_draft,
    current_session_id,
    ..
} = self;
```

Then, inside the `KeyCode::Enter => { if *cursor == 0 { ... } else { ... }` branch (around line 885), modify the `else` branch's body. Current body:

```rust
let indices = Self::filtered_indices(sessions, filter, metadata, *show_archived);
let real_idx = indices.get(*cursor - 1).copied()?;
let sid = sessions[real_idx].session_id.0.to_string();
// Confirm only when the draft belongs to a DIFFERENT
// session than the one being resumed.
let draft_elsewhere = current_session_with_draft
    .as_ref()
    .map(|cur| cur != &sid)
    .unwrap_or(false);
if draft_elsewhere {
    post = Post::StartConfirmSwitch(ConfirmSwitchTarget::Resume(sid));
    None
} else {
    *resuming = true;
    Some(Action::ResumeSession { session_id: sid })
}
```

Replace with:

```rust
let indices = Self::filtered_indices(sessions, filter, metadata, *show_archived);
let real_idx = indices.get(*cursor - 1).copied()?;
let sid = sessions[real_idx].session_id.0.to_string();

if current_session_id.as_deref() == Some(sid.as_str()) {
    // Short-circuit: the selected row IS the currently-active session.
    // Don't re-resume — just navigate back to its detail view. No
    // backend traffic; no confirm-switch banner (there's nothing to
    // switch away from).
    Some(Action::NavigateTo(ViewId::SessionDetail(
        spur_acp::SessionId(sid),
    )))
} else {
    // Confirm only when the draft belongs to a DIFFERENT session than
    // the one being resumed.
    let draft_elsewhere = current_session_with_draft
        .as_ref()
        .map(|cur| cur != &sid)
        .unwrap_or(false);
    if draft_elsewhere {
        post = Post::StartConfirmSwitch(ConfirmSwitchTarget::Resume(sid));
        None
    } else {
        *resuming = true;
        Some(Action::ResumeSession { session_id: sid })
    }
}
```

If `ViewId` and `spur_acp::SessionId` are not already in scope at this file, add imports near the top:

```rust
// Already likely present:
use crate::action::{Action, ViewId};
use spur_acp::SessionId;
```

Check existing imports before adding duplicates.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: clean.

- [ ] **Step 4: Write a unit test for the short-circuit**

Append to the existing `#[cfg(test)] mod` section at the end of `session_picker.rs` (or add a new one if none exists). Test body:

```rust
#[cfg(test)]
mod current_session_shortcut_tests {
    use super::*;
    use crate::action::{Action, ViewId};
    use spur_acp::{SessionId, SessionInfo};
    use std::path::PathBuf;

    fn make_session(id: &str) -> SessionInfo {
        // Construct a minimal SessionInfo. If the SDK exposes a builder
        // or a Default impl, use it and set only the session_id. Fields
        // may need adjustment to match the actual SessionInfo struct —
        // check `spur_acp::SessionInfo` and the existing test fixtures
        // in session_picker.rs for an example.
        SessionInfo {
            session_id: SessionId(id.to_string()),
            cwd: PathBuf::from("/tmp"),
            // Add any other required fields here, defaulted.
            ..Default::default()
        }
    }

    #[test]
    fn enter_on_current_session_row_navigates_back() {
        let mut picker = SessionPickerView::new();
        picker.set_sessions("test-brain".into(), vec![make_session("A")]);
        picker.set_current_session_id(Some("A".into()));

        // Cursor starts at 0 (the [+ New session] row); move to 1 (the A row).
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            Some(Action::NavigateTo(ViewId::SessionDetail(sid))) => {
                assert_eq!(sid.0, "A");
            }
            other => panic!(
                "expected NavigateTo(SessionDetail(A)), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn enter_on_different_session_row_still_resumes() {
        let mut picker = SessionPickerView::new();
        picker.set_sessions(
            "test-brain".into(),
            vec![make_session("A"), make_session("B")],
        );
        picker.set_current_session_id(Some("A".into()));

        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // Move cursor to row index 2 = session B.
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            Some(Action::ResumeSession { session_id }) => {
                assert_eq!(session_id, "B");
            }
            other => panic!("expected ResumeSession(B), got {:?}", other),
        }
    }
}
```

If `SessionInfo` does not implement `Default`, remove the `..Default::default()` and list all required fields explicitly (inspect the struct definition in the upstream `agent_client_protocol` crate or check how session_picker's existing tests — if any — construct it).

- [ ] **Step 5: Run test**

Run: `cargo test -p spur-tui current_session_shortcut -- --nocapture`
Expected: both tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): picker short-circuits Enter on current session to NavigateTo"
```

---

## Task 4: Push `current_session_id` from App to the picker

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Update `refresh_picker_metadata`**

Find the method at approximately line 948–954:

```rust
fn refresh_picker_metadata(&mut self) {
    let draft = self.compute_draft_session();
    if let Some(ref mut picker) = self.session_picker {
        picker.set_metadata(self.metadata_store.metadata().clone());
        picker.set_current_session_has_draft(draft);
    }
}
```

Replace with:

```rust
fn refresh_picker_metadata(&mut self) {
    let draft = self.compute_draft_session();
    let current = self
        .session_detail
        .as_ref()
        .map(|d| d.session_id().0.clone());
    if let Some(ref mut picker) = self.session_picker {
        picker.set_metadata(self.metadata_store.metadata().clone());
        picker.set_current_session_has_draft(draft);
        picker.set_current_session_id(current);
    }
}
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p spur-tui`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): push current_session_id into picker on refresh"
```

---

## Task 5: Fix `Action::NewSessionRequested` stub

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Locate the stub**

Around line 707 in `crates/spur-tui/src/app.rs`:

```rust
Action::NewSessionRequested => {
    // Stub until Task 15 (BUG-2 fix) adds NewSessionWithMessage plumbing.
    // For now, dismiss picker by navigating to Dashboard.
    self.current_view = ViewId::Dashboard;
    self.dirty = true;
}
```

- [ ] **Step 2: Replace with the real handler**

```rust
Action::NewSessionRequested => {
    // Shut down the current brain atomically so picker [+ New session]
    // doesn't leave the old agent subprocess's session running.
    // Orchestrator's NewSessionWithMessage arm with empty blocks is
    // defined as "retire current brain, defer spawn to next Message."
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

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "fix(spur-tui): NewSessionRequested shuts down current brain"
```

---

## Task 6: Orchestrator `retire_active_brain` helper + unified cleanup

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add the helper method on `Orchestrator`**

Find an existing `impl Orchestrator { ... }` block in `crates/spur-core/src/orchestrator.rs` that contains `pub async fn run_interactive`. Add this private method to that `impl` block (it doesn't take `&mut self` because it only mutates the caller's locals):

```rust
/// Retire the currently-active brain session's ephemeral state
/// (delegation handler task, MCP server) while preserving the
/// initialized ACP connection in `agent_connection` for reuse by the
/// next `load_brain_session` / `create_brain_session`.
///
/// Called at the top of any arm that replaces the current brain
/// (`ResumeSession`, `NewSessionWithMessage`). Saves the cost of
/// tearing down and reinitializing the agent subprocess on every
/// session switch — for claude-code-acp that's ~1–3s of node startup
/// per switch.
///
/// The old ACP session id on the agent side is abandoned silently;
/// the ACP protocol has no `close_session` and most agents treat
/// unreferenced sessions as inert.
fn retire_active_brain(
    brain: &mut Option<BrainSession>,
    agent_connection: &mut Option<(Box<dyn spur_acp::AgentConnection>, String)>,
) {
    if let Some(b) = brain.take() {
        b.delegation_handle.abort();
        let _ = b.mcp_server.shutdown();
        *agent_connection = Some((b.connection, b.brain_name));
    }
}
```

Note: `mcp_server` is `Arc<McpCallbackServer>` and `shutdown` is callable on `&Arc<T>` if `T::shutdown` takes `&self`. Verify by grepping `fn shutdown` on `McpCallbackServer`. If it takes `&mut self`, use `Arc::try_unwrap` or add an inherent shutdown method that works on `Arc<Self>`. If the existing `NewSessionWithMessage` arm (line ~751) already calls `b.mcp_server.shutdown()`, the call is correct as-is.

- [ ] **Step 2: Replace the inline cleanup in `ResumeSession` arm**

The current `ResumeSession` arm (around line 447) starts with:

```rust
InteractiveInput::ResumeSession { session_id } => {
    // Use pre-connected or connect fresh.
    let (connection, brain_name) = match agent_connection.take() {
        Some(existing) => existing,
        None => {
            match self
                .connect_brain(brain_override.as_deref(), permission_tx.clone())
                .await
            {
                ...
```

Insert the retire call as the FIRST statement in this arm, before the `match agent_connection.take()`:

```rust
InteractiveInput::ResumeSession { session_id } => {
    // If a brain is already active, retire its session-level state so
    // the incoming ResumeSession replaces it cleanly.
    Self::retire_active_brain(&mut brain, &mut agent_connection);

    let (connection, brain_name) = match agent_connection.take() {
        Some(existing) => existing,
        // ... rest unchanged
```

- [ ] **Step 3: Replace the inline cleanup in `NewSessionWithMessage` arm**

The current arm (around line 748–761) is:

```rust
InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
    if let Some(mut b) = brain.take() {
        b.delegation_handle.abort();
        let _ = b.connection.shutdown().await;
        let _ = b.mcp_server.shutdown();
    }
    if blocks.is_empty() {
        // Spawn-only: no prompt. Leave brain=None; the next
        // Message will lazy-spawn.
        info!("NewSessionWithMessage with empty blocks — spawn deferred to next Message");
    } else {
        pending_messages.push_back(InteractiveInput::Message { blocks, interrupt });
    }
}
```

Replace with:

```rust
InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
    // Retire the active brain (if any) but preserve the initialized
    // connection for the next Message arm's lazy-spawn to reuse.
    Self::retire_active_brain(&mut brain, &mut agent_connection);

    if blocks.is_empty() {
        // Spawn-only: no prompt. Leave brain=None; the next Message
        // will lazy-spawn using the preserved agent_connection.
        info!("NewSessionWithMessage with empty blocks — spawn deferred to next Message");
    } else {
        pending_messages.push_back(InteractiveInput::Message { blocks, interrupt });
    }
}
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p spur-core`
Expected: clean.

Run: `cargo check --workspace`
Expected: clean across all crates.

- [ ] **Step 5: Verify no tests regress**

Run: `cargo test -p spur-core`
Expected: all existing tests still pass. If any test that constructed a `BrainSession` and dispatched `NewSessionWithMessage` relied on the OLD behavior (connection fully shut down), it'll need updating — but the observable difference is "agent subprocess stays alive vs. is killed," which no existing spur-core test should assert on.

Run: `cargo test -p spur-acp --test load_session_error_propagation`
Expected: PASS (Fix #1 regression still green).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "fix(spur-core): unify brain cleanup via retire_active_brain, preserve connection across session switches"
```

---

## Task 7: End-to-end verification

**Files:** none (manual smoke).

- [ ] **Step 1: Build**

Run: `cargo build --workspace`
Expected: clean.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: all green, including Task 1's `/sessions` routing test and Task 3's two picker tests.

- [ ] **Step 3: Run the TUI with two sessions**

First, ensure `.spur/session_metadata.json` has at least one pre-existing session (or run spur once to seed one). Then:

```bash
cargo run -p spur-cli -- watch --brain claude-code-acp
```

- [ ] **Step 4: Verify the switch flow**

1. Once auto-resume lands you in a session (session A), type "hello" and submit. Wait for the first `TurnComplete`.
2. Press `Alt+s`. The picker should open. Verify "[+ New session]" is the first row and sessions are listed below.
3. With cursor on session A's row, press Enter. You should navigate back to session A's detail view with no flicker, no new `BrainSpawned` event in the log, no new claude-code-acp process spawned.
4. Press `Alt+s` again, move cursor to a different session B's row, press Enter. The picker closes, the trace switches to B, and `.spur/logs/claude-code-acp-*.log` shows a `session/load` for B's ACP id succeeding (no `-32002`). Verify `ps aux | grep node | grep claude-code-acp | wc -l` shows exactly 1.
5. Press `Alt+s`, Enter on the `[+ New session]` row. Observe: land on Dashboard with empty input bar, no claude-code-acp subprocess running yet. Type a new message and submit — a fresh agent spawns.

- [ ] **Step 5: Verify `/sessions` slash command equivalence**

From SessionDetail, type `/sessions` and Enter. Same behavior as Alt+s.

- [ ] **Step 6: Tag the completion**

```bash
# No commit needed for a smoke test; record the final HEAD for reference.
git rev-parse --short HEAD
```

If any of Steps 3–5 fail, return to the task that introduced the failure:
- Picker doesn't open on Alt+s → Task 2.
- Picker opens but wrong behavior on Enter → Task 3 or 4.
- New-session row doesn't tear down the brain → Task 5.
- Switch leaks a subprocess or re-resumes wastefully → Task 6.

---

## Summary of commits produced

1. `feat(spur-tui): add /sessions slash command`
2. `feat(spur-tui): Alt+s opens session picker from SessionDetail`
3. `feat(spur-tui): picker short-circuits Enter on current session to NavigateTo`
4. `feat(spur-tui): push current_session_id into picker on refresh`
5. `fix(spur-tui): NewSessionRequested shuts down current brain`
6. `fix(spur-core): unify brain cleanup via retire_active_brain, preserve connection across session switches`
