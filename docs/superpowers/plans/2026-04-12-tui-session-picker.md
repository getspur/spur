# TUI Session Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a TUI session picker that lets users browse and resume previous ACP agent sessions via `[s]` keybinding or `--sessions` flag.

**Architecture:** New `SessionPickerView` implementing the existing `View` trait. Data flows through existing TUI ↔ Orchestrator event channels — `UserInput`/`InteractiveInput` enums carry session commands, `SpurEvent` carries results. The orchestrator's `spawn_brain_session` is decomposed into `connect_brain` + `create_brain_session` + `load_brain_session` to support connecting without creating a session.

**Tech Stack:** Rust, ratatui, crossterm, tokio, agent_client_protocol (ACP SDK)

**Spec:** `docs/superpowers/specs/2026-04-12-tui-session-picker.md`

---

## File Map

| File | Responsibility |
|------|---------------|
| `crates/spur-acp/src/domain/events.rs` | SpurEvent enum — add `SessionsListed`, `SessionsListError` |
| `crates/spur-acp/src/lib.rs` | Re-export `SessionInfo` from ACP SDK |
| `crates/spur-tui/src/action.rs` | Action/ViewId enums — add picker variants |
| `crates/spur-tui/src/views/session_picker.rs` | **New.** SessionPickerView with 4 states |
| `crates/spur-tui/src/views/mod.rs` | Register session_picker module |
| `crates/spur-tui/src/views/dashboard.rs` | `[s]` keybinding + SpurEvent catchall |
| `crates/spur-tui/src/components/status_bar.rs` | SessionPicker hints |
| `crates/spur-tui/src/app.rs` | UserInput enum + picker integration + run_tui param |
| `crates/spur-tui/src/lib.rs` | Re-export updated types |
| `crates/spur-core/src/orchestrator.rs` | InteractiveInput enum + connect_brain refactor + handlers |
| `crates/spur-core/src/lib.rs` | Re-export updated types |
| `crates/spur-cli/src/main.rs` | `--sessions` flag + forwarding task update |

---

### Task 1: Add SpurEvent variants and SessionInfo re-export

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs:1-28`
- Modify: `crates/spur-acp/src/lib.rs:19-27`

- [ ] **Step 1: Add SessionInfo to spur-acp re-exports**

In `crates/spur-acp/src/lib.rs`, add `SessionInfo` and `ListSessionsRequest` to the `agent_client_protocol` re-export block:

```rust
pub use agent_client_protocol::{
    ContentBlock, ContentChunk, TextContent,
    SessionNotification, SessionUpdate,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
    RequestPermissionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionInfo, ListSessionsRequest, ListSessionsResponse,
};
```

- [ ] **Step 2: Add SessionsListed and SessionsListError to SpurEvent**

In `crates/spur-acp/src/domain/events.rs`, add the import and two new variants:

```rust
use agent_client_protocol::{SessionNotification, SessionInfo};
```

Add before the closing `}` of the enum, after `BrainError`:

```rust
    // ── Session picker events ───────────────────────────────────────
    SessionsListed { agent: String, sessions: Vec<SessionInfo> },
    SessionsListError { message: String },
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p spur-acp`
Expected: compiles with no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): add SessionsListed/SessionsListError events and SessionInfo re-export"
```

---

### Task 2: Add Action and ViewId variants

**Files:**
- Modify: `crates/spur-tui/src/action.rs:1-43`

- [ ] **Step 1: Add ViewId::SessionPicker**

In `crates/spur-tui/src/action.rs`, add to the `ViewId` enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
    SessionPicker,
}
```

- [ ] **Step 2: Add Action::RequestSessions and Action::ResumeSession**

Add to the `Action` enum:

```rust
pub enum Action {
    Quit,
    NavigateTo(ViewId),
    NavigateBack,
    SendMessage {
        session: SessionId,
        text: String,
        interrupt: bool,
    },
    ToggleVerbose,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    CycleFocus,
    ShowHelp,
    HideHelp,
    Tick,
    PermissionGrant(PermissionChoice),
    RequestSessions,
    ResumeSession { session_id: String },
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p spur-tui 2>&1 | head -30`
Expected: compilation errors from exhaustive matches on `ViewId` in `status_bar.rs` and `app.rs`. This is expected — we fix those in later tasks.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): add SessionPicker view ID and session actions"
```

---

### Task 3: Fix exhaustive matches — StatusBar and Dashboard

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs:21-24`
- Modify: `crates/spur-tui/src/views/dashboard.rs:297-529`

- [ ] **Step 1: Add ViewId::SessionPicker arm to StatusBar**

In `crates/spur-tui/src/components/status_bar.rs`, update the `hints` match at line 21:

```rust
        let hints = match view {
            ViewId::Dashboard => " [i]nput [Enter]session [s]essions [?]help [q]uit",
            ViewId::SessionDetail(_) => " [Enter]send [Esc]back [j/k]scroll [?]help",
            ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
        };
```

Note: `[s]essions` added to Dashboard hints to surface the feature. The Unicode escapes are ↑↓ arrows.

- [ ] **Step 2: Add catchall to DashboardView::handle_spur_event**

In `crates/spur-tui/src/views/dashboard.rs`, after the `BrainError` match arm (line ~528), add before the closing `}`:

```rust
            // Session picker events are handled by App, not Dashboard.
            _ => {}
```

- [ ] **Step 3: Add `[s]` keybinding to Dashboard**

In `crates/spur-tui/src/views/dashboard.rs`, in the `handle_key` method, inside the `if self.input_bar.text().len() == 1` block (around line 218), add a new arm to the char match:

```rust
                    's' => {
                        self.input_bar.clear();
                        return Some(Action::RequestSessions);
                    }
```

Add it after the `'?'` arm and before the `_ => {}` arm.

- [ ] **Step 4: Add hint to splash screen**

In `crates/spur-tui/src/views/dashboard.rs`, in the `render` method, inside the `if self.agents.is_empty()` block (around line 569), update the splash text. After the "Type a task below to start" line, add:

```rust
                Line::from(Span::styled(
                    "Press [s] to resume a session",
                    Style::default().fg(Color::DarkGray),
                )),
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p spur-tui 2>&1 | head -20`
Expected: remaining errors from `app.rs` exhaustive matches on `ViewId` (fixed in Task 6). No errors from status_bar or dashboard.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): add [s] keybinding, status bar hints, and dashboard catchall"
```

---

### Task 4: Create SessionPickerView

**Files:**
- Create: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/views/mod.rs`

- [ ] **Step 1: Register module**

In `crates/spur-tui/src/views/mod.rs`, add:

```rust
pub mod dashboard;
pub mod session_detail;
pub mod session_picker;
```

- [ ] **Step 2: Create SessionPickerView**

Create `crates/spur-tui/src/views/session_picker.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{SessionInfo, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::status_bar::StatusBar;

use super::View;

// ─── State ────────────────────────────────────────────────────────────

enum PickerState {
    Loading,
    Populated {
        agent: String,
        sessions: Vec<SessionInfo>,
        cursor: usize,
        resuming: bool,
    },
    Empty {
        agent: String,
    },
    Error {
        message: String,
    },
}

// ─── View ─────────────────────────────────────────────────────────────

pub struct SessionPickerView {
    state: PickerState,
    scroll_offset: usize,
}

impl SessionPickerView {
    pub fn new() -> Self {
        Self {
            state: PickerState::Loading,
            scroll_offset: 0,
        }
    }

    /// Update state when sessions arrive.
    pub fn set_sessions(&mut self, agent: String, sessions: Vec<SessionInfo>) {
        if sessions.is_empty() {
            self.state = PickerState::Empty { agent };
        } else {
            self.state = PickerState::Populated {
                agent,
                sessions,
                cursor: 0,
                resuming: false,
            };
        }
        self.scroll_offset = 0;
    }

    /// Update state on error.
    pub fn set_error(&mut self, message: String) {
        self.state = PickerState::Error { message };
    }

    /// Format elapsed time as relative string.
    fn relative_time(iso: &str) -> String {
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
            return String::new();
        };
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        let secs = diff.num_seconds();
        if secs < 60 {
            "just now".to_string()
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 86400 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }

    /// Check if sessions span multiple working directories.
    fn cwds_are_heterogeneous(sessions: &[SessionInfo]) -> bool {
        if sessions.len() <= 1 {
            return false;
        }
        let first = &sessions[0].cwd;
        sessions.iter().any(|s| s.cwd != *first)
    }

    /// Extract the last path component from a cwd string.
    fn cwd_basename(cwd: &str) -> &str {
        std::path::Path::new(cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(cwd)
    }

    /// Build display text for a session row.
    fn display_text(session: &SessionInfo, show_cwd: bool) -> String {
        if let Some(ref title) = session.title {
            title.clone()
        } else if show_cwd {
            format!("{}/", Self::cwd_basename(&session.cwd))
        } else {
            "(untitled session)".to_string()
        }
    }

    fn render_loading(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  Connecting to agent"),
                Span::styled(" \u{00b7}\u{00b7}\u{00b7}", Style::default().fg(Color::Cyan)),
            ]),
        ];
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);

        let v_pad = chunks[0].height.saturating_sub(4) / 3;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };
        frame.render_widget(Paragraph::new(lines), content_area);
        StatusBar::render(frame, chunks[1], &ViewId::SessionPicker, 0.0, "0m 00s");
    }

    fn render_populated(
        &self,
        frame: &mut Frame,
        area: Rect,
        agent: &str,
        sessions: &[SessionInfo],
        cursor: usize,
        resuming: bool,
    ) {
        let show_cwd = Self::cwds_are_heterogeneous(sessions);
        let visible_height = area.height.saturating_sub(4) as usize; // header + footer + status

        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "Sessions ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({})", agent),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(""),
        ];

        for (i, session) in sessions.iter().enumerate().skip(self.scroll_offset).take(visible_height) {
            let is_selected = i == cursor;
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let short_id = &session.session_id[..8.min(session.session_id.len())];
            let display = Self::display_text(session, show_cwd);
            let time_str = session
                .updated_at
                .as_deref()
                .map(Self::relative_time)
                .unwrap_or_default();

            let suffix = if is_selected && resuming {
                " loading...".to_string()
            } else if show_cwd {
                let basename = Self::cwd_basename(&session.cwd);
                format!("  {}/", basename)
            } else {
                String::new()
            };

            let style = if is_selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };

            let id_style = if is_selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(short_id, id_style),
                Span::styled(" \u{00b7} ", Style::default().fg(Color::DarkGray)),
                Span::styled(display, style),
                Span::styled(suffix, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(time_str, Style::default().fg(Color::DarkGray)),
            ]));
        }

        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);

        frame.render_widget(Paragraph::new(lines), chunks[0]);
        StatusBar::render(frame, chunks[1], &ViewId::SessionPicker, 0.0, "0m 00s");
    }

    fn render_empty(&self, frame: &mut Frame, area: Rect, agent: &str) {
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "Sessions ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({})", agent),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  No saved sessions found.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Start a new conversation from the dashboard.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);
        let v_pad = chunks[0].height.saturating_sub(5) / 3;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };
        frame.render_widget(Paragraph::new(lines), content_area);
        StatusBar::render(frame, chunks[1], &ViewId::SessionPicker, 0.0, "0m 00s");
    }

    fn render_error(&self, frame: &mut Frame, area: Rect, message: &str) {
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", message),
                Style::default().fg(Color::Red),
            )),
            Line::from(Span::styled(
                "  Use --resume <id> to load a session by ID.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);
        let v_pad = chunks[0].height.saturating_sub(5) / 3;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };
        frame.render_widget(Paragraph::new(lines), content_area);
        StatusBar::render(frame, chunks[1], &ViewId::SessionPicker, 0.0, "0m 00s");
    }
}

impl View for SessionPickerView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match &mut self.state {
            PickerState::Populated {
                sessions,
                cursor,
                resuming,
                ..
            } => {
                if *resuming {
                    return None; // ignore input while loading session
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *cursor > 0 {
                            *cursor -= 1;
                            if *cursor < self.scroll_offset {
                                self.scroll_offset = *cursor;
                            }
                        }
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *cursor + 1 < sessions.len() {
                            *cursor += 1;
                            // scroll_offset adjusted in render based on visible_height
                        }
                        None
                    }
                    KeyCode::Enter => {
                        let sid = sessions[*cursor].session_id.clone();
                        *resuming = true;
                        Some(Action::ResumeSession { session_id: sid })
                    }
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
                }
            }
            PickerState::Loading | PickerState::Empty { .. } | PickerState::Error { .. } => {
                match key.code {
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
                }
            }
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent) {
        // SessionsListed and SessionsListError are handled by App,
        // which calls set_sessions() or set_error() directly.
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match &self.state {
            PickerState::Loading => self.render_loading(frame, area),
            PickerState::Populated {
                agent,
                sessions,
                cursor,
                resuming,
            } => self.render_populated(frame, area, agent, sessions, *cursor, *resuming),
            PickerState::Empty { agent } => self.render_empty(frame, area, agent),
            PickerState::Error { message } => self.render_error(frame, area, message),
        }
    }

    fn tick(&mut self) {
        // No animations in the picker.
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p spur-tui 2>&1 | head -20`
Expected: errors from `app.rs` (ViewId::SessionPicker not handled yet). session_picker.rs itself should have no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/views/mod.rs
git commit -m "feat(spur-tui): add SessionPickerView with 4 states and adaptive layout"
```

---

### Task 5: Convert UserInput to enum and integrate picker into App

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1-481`
- Modify: `crates/spur-tui/src/lib.rs`

- [ ] **Step 1: Convert UserInput to enum**

In `crates/spur-tui/src/app.rs`, replace the `UserInput` struct (lines 21-25):

```rust
/// A message from the TUI to the orchestrator.
pub enum UserInput {
    Message {
        session: SessionId,
        text: String,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
}
```

- [ ] **Step 2: Add session_picker field to App**

In `crates/spur-tui/src/app.rs`, add the import and field. Add to imports:

```rust
use crate::views::session_picker::SessionPickerView;
```

Add field to `App` struct (after `session_detail`):

```rust
    session_picker: Option<SessionPickerView>,
```

Initialize in `App::new`:

```rust
    session_picker: None,
```

- [ ] **Step 3: Add `start_in_picker` parameter to App::new and run_tui**

Update `App::new` signature to accept `start_in_picker: bool`:

```rust
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>, start_in_picker: bool) -> Self {
```

When `start_in_picker` is true, set initial view and create picker:

```rust
        let (current_view, session_picker) = if start_in_picker {
            (ViewId::SessionPicker, Some(SessionPickerView::new()))
        } else {
            (ViewId::Dashboard, None)
        };

        Self {
            current_view,
            dashboard: DashboardView::new(),
            session_detail: None,
            session_picker,
            // ... rest of fields unchanged
        }
```

After creating Self, if `start_in_picker`, send the ListSessions command:

```rust
        let mut app = Self { /* ... */ };
        if start_in_picker {
            if let Some(ref tx) = app.user_input_tx {
                let _ = tx.try_send(UserInput::ListSessions);
            }
        }
        app
```

- [ ] **Step 4: Update run_tui to accept start_in_picker**

Update the `run_tui` function signature:

```rust
pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
) -> anyhow::Result<()> {
```

Update the `App::new` call inside:

```rust
    let mut app = App::new(user_input_tx, start_in_picker);
```

- [ ] **Step 5: Handle ViewId::SessionPicker in handle_crossterm_event**

In `handle_crossterm_event`, add the `SessionPicker` arm in the key match (around line 86):

```rust
                let action = match self.current_view {
                    ViewId::Dashboard => self.dashboard.handle_key(key),
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.handle_key(key)
                        } else {
                            None
                        }
                    }
                    ViewId::SessionPicker => {
                        if let Some(ref mut picker) = self.session_picker {
                            picker.handle_key(key)
                        } else {
                            None
                        }
                    }
                };
```

- [ ] **Step 6: Handle ViewId::SessionPicker in handle_mouse_event**

In `handle_mouse_event`, add the arm (no scrolling for picker in v1):

```rust
            ViewId::SessionPicker => {
                // No mouse scroll in session picker v1.
            }
```

- [ ] **Step 7: Handle SessionsListed and SessionsListError in handle_spur_event**

Add new match arms in `handle_spur_event`, before the `// Forward to views` section:

```rust
            SpurEvent::SessionsListed { ref agent, ref sessions } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_sessions(agent.clone(), sessions.clone());
                }
            }
            SpurEvent::SessionsListError { ref message } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_error(message.clone());
                }
            }
```

- [ ] **Step 8: Extend BrainSpawned auto-navigate**

In `handle_spur_event`, update the BrainSpawned auto-navigate condition (around line 173):

```rust
                if matches!(self.current_view, ViewId::Dashboard | ViewId::SessionPicker) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
```

- [ ] **Step 9: Handle new Actions in process_action**

Add new arms in `process_action`:

```rust
            Action::RequestSessions => {
                self.session_picker = Some(SessionPickerView::new());
                self.current_view = ViewId::SessionPicker;
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ListSessions);
                }
            }

            Action::ResumeSession { session_id } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumeSession { session_id });
                }
            }

            Action::NavigateTo(ViewId::SessionPicker) => {
                // Shouldn't happen from other views, but handle gracefully.
                self.current_view = ViewId::SessionPicker;
            }
```

- [ ] **Step 10: Update SendMessage action to use new UserInput enum**

In `process_action`, update the `Action::SendMessage` arm to use the enum variant:

```rust
            Action::SendMessage {
                session,
                text,
                interrupt,
            } => {
                // ... brain_status transition unchanged ...

                if let Some(ref mut detail) = self.session_detail {
                    detail.push_user_message(&text);
                } else {
                    self.pending_user_messages.push(text.clone());
                }

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput::Message {
                        session,
                        text,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
                }

                self.sync_brain_status();
            }
```

- [ ] **Step 11: Handle ViewId::SessionPicker in render**

Update the `render` method:

```rust
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        match self.current_view {
            ViewId::Dashboard => self.dashboard.render(frame, area),
            ViewId::SessionDetail(_) => {
                if let Some(ref detail) = self.session_detail {
                    detail.render(frame, area);
                }
            }
            ViewId::SessionPicker => {
                if let Some(ref picker) = self.session_picker {
                    picker.render(frame, area);
                }
            }
        }

        if self.help_visible {
            HelpOverlay::render(frame, area);
        }
    }
```

- [ ] **Step 12: Handle ViewId::SessionPicker in tick**

Update the `tick` method:

```rust
            ViewId::SessionPicker => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.tick();
                }
            }
```

- [ ] **Step 13: Verify compilation**

Run: `cargo check -p spur-tui`
Expected: compiles successfully. All ViewId exhaustive matches handled.

- [ ] **Step 14: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/lib.rs
git commit -m "feat(spur-tui): integrate SessionPicker into App with UserInput enum"
```

---

### Task 6: Convert InteractiveInput to enum and refactor orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:60-64` (InteractiveInput)
- Modify: `crates/spur-core/src/orchestrator.rs:596-698` (spawn_brain_session decomposition)
- Modify: `crates/spur-core/src/orchestrator.rs:312-430` (run_interactive)
- Modify: `crates/spur-core/src/lib.rs`

- [ ] **Step 1: Convert InteractiveInput to enum**

In `crates/spur-core/src/orchestrator.rs`, replace the struct (lines 60-64):

```rust
/// Commands from the TUI to the orchestrator.
pub enum InteractiveInput {
    Message { text: String, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
}
```

- [ ] **Step 2: Extract connect_brain from spawn_brain_session**

Add a new method before `spawn_brain_session`:

```rust
    /// Phase 1: Resolve brain agent, create connection, initialize.
    /// Returns an initialized connection with no session.
    async fn connect_brain(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<(Box<dyn AgentConnection>, String)> {
        let brain_name = brain_override
            .unwrap_or(&self.config.brain.default)
            .to_string();

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        info!(brain = %brain_name, "Connecting to brain agent");

        let mut connection = self.create_connection(&brain_config, permission_tx);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(brain = %brain_name, "Brain agent initialized");

        Ok((connection, brain_name))
    }
```

- [ ] **Step 3: Extract create_brain_session (Phase 2a)**

Add a new method that takes an already-initialized connection and creates a new session:

```rust
    /// Phase 2a: Start MCP server, create new session, start delegation handler.
    async fn create_brain_session(
        &mut self,
        connection: Box<dyn AgentConnection>,
        brain_name: String,
    ) -> Result<BrainSession> {
        let session_id = SessionId::new();

        self.emit(SpurEvent::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        });

        // Start MCP callback server.
        let (mcp_server, delegation_channel) = McpCallbackServer::new(&session_id);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .iter()
            .map(|c| WorkerInfo {
                name: c.name.clone(),
                description: c.capabilities.join(", "),
                cost_tier: c.cost_tier,
            })
            .collect();
        mcp_server.set_workers(workers);

        let mcp_endpoint = mcp_server.endpoint();
        let mcp_server = Arc::new(mcp_server);
        let _mcp_handle = mcp_server
            .clone()
            .start()
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                &brain_name,
                "brain",
                None,
                "(interactive)",
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        // Create session on agent.
        let mut connection = connection;
        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        let session_response = connection
            .new_session(self.repo_root.clone(), mcp_servers)
            .await
            .context("Failed to create brain session")?;

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
        ));

        Ok(BrainSession {
            connection,
            acp_session_id: session_response.session_id.to_string(),
            spur_session_id: session_id,
            brain_name,
            mcp_server,
            delegation_handle,
        })
    }
```

- [ ] **Step 4: Add load_brain_session (Phase 2b)**

```rust
    /// Phase 2b: Start MCP server, load existing session, start delegation handler.
    async fn load_brain_session(
        &mut self,
        connection: Box<dyn AgentConnection>,
        brain_name: String,
        session_id_to_load: &str,
    ) -> Result<(BrainSession, Pin<Box<dyn Stream<Item = agent_client_protocol::SessionNotification> + Send>>)> {
        let spur_session_id = SessionId::new();

        self.emit(SpurEvent::BrainSpawned {
            agent: brain_name.clone(),
            session: spur_session_id.clone(),
        });

        // Start MCP callback server.
        let (mcp_server, delegation_channel) = McpCallbackServer::new(&spur_session_id);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .iter()
            .map(|c| WorkerInfo {
                name: c.name.clone(),
                description: c.capabilities.join(", "),
                cost_tier: c.cost_tier,
            })
            .collect();
        mcp_server.set_workers(workers);

        let mcp_endpoint = mcp_server.endpoint();
        let mcp_server = Arc::new(mcp_server);
        let _mcp_handle = mcp_server
            .clone()
            .start()
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &spur_session_id,
                &brain_name,
                "brain",
                None,
                "(resumed)",
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        // Load session on agent.
        let mut connection = connection;
        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        use agent_client_protocol::LoadSessionRequest;
        let load_request = LoadSessionRequest::new(session_id_to_load.to_string())
            .cwd(self.repo_root.to_string_lossy().to_string())
            .mcp_servers(mcp_servers);

        let history_stream = connection
            .load_session(load_request)
            .await
            .context("Failed to load session")?;

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
        ));

        let brain_session = BrainSession {
            connection,
            acp_session_id: session_id_to_load.to_string(),
            spur_session_id,
            brain_name,
            mcp_server,
            delegation_handle,
        };

        Ok((brain_session, history_stream))
    }
```

- [ ] **Step 5: Update spawn_brain_session to use connect_brain + create_brain_session**

Replace the body of `spawn_brain_session` to delegate:

```rust
    pub async fn spawn_brain_session(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<BrainSession> {
        let (connection, brain_name) = self.connect_brain(brain_override, permission_tx).await?;
        self.create_brain_session(connection, brain_name).await
    }
```

- [ ] **Step 6: Update run_interactive to use enum and add session handlers**

Rewrite `run_interactive` to handle the new enum variants. Add `agent_connection` local state and match on input variants. The key changes:

At the top of `run_interactive`, add:

```rust
        let mut agent_connection: Option<(Box<dyn AgentConnection>, String)> = None;
```

Replace the input-receiving section to match on InteractiveInput variants. The `Message` arm contains the existing prompt logic. Add new arms:

```rust
                InteractiveInput::ListSessions => {
                    // Connect to brain if not already connected.
                    if agent_connection.is_none() {
                        match self.connect_brain(brain_override.as_deref(), permission_tx.clone()).await {
                            Ok(conn) => agent_connection = Some(conn),
                            Err(e) => {
                                error!(error = %e, "Failed to connect for session listing");
                                self.emit(SpurEvent::SessionsListError {
                                    message: e.to_string(),
                                });
                                continue;
                            }
                        }
                    }

                    let (ref mut conn, ref agent_name) = agent_connection.as_mut().unwrap();
                    use agent_client_protocol::ListSessionsRequest;
                    match conn.list_sessions(ListSessionsRequest::new()).await {
                        Ok(response) => {
                            self.emit(SpurEvent::SessionsListed {
                                agent: agent_name.clone(),
                                sessions: response.sessions,
                            });
                        }
                        Err(e) => {
                            self.emit(SpurEvent::SessionsListError {
                                message: e.to_string(),
                            });
                        }
                    }
                }

                InteractiveInput::ResumeSession { session_id } => {
                    let (conn, brain_name) = match agent_connection.take() {
                        Some(c) => c,
                        None => {
                            match self.connect_brain(brain_override.as_deref(), permission_tx.clone()).await {
                                Ok(c) => c,
                                Err(e) => {
                                    error!(error = %e, "Failed to connect for session resume");
                                    self.emit(SpurEvent::BrainError {
                                        session: SessionId::new(),
                                        message: e.to_string(),
                                    });
                                    continue;
                                }
                            }
                        }
                    };

                    match self.load_brain_session(conn, brain_name, &session_id).await {
                        Ok((b, mut history_stream)) => {
                            // Drain history stream.
                            while let Some(notification) = history_stream.next().await {
                                self.emit(SpurEvent::AgentNotification {
                                    session: b.spur_session_id.clone(),
                                    notification,
                                });
                            }
                            self.emit(SpurEvent::TurnComplete {
                                session: b.spur_session_id.clone(),
                            });
                            brain = Some(b);
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to load session");
                            self.emit(SpurEvent::BrainError {
                                session: SessionId::new(),
                                message: e.to_string(),
                            });
                        }
                    }
                }
```

For the `Message` arm, update the brain spawn to use `agent_connection` if available:

```rust
                InteractiveInput::Message { text, interrupt: _ } => {
                    // ... pending_messages logic unchanged ...

                    if brain.is_none() {
                        let result = if let Some((conn, name)) = agent_connection.take() {
                            self.create_brain_session(conn, name).await
                        } else {
                            self.spawn_brain_session(brain_override.as_deref(), permission_tx.clone()).await
                        };
                        match result {
                            Ok(b) => brain = Some(b),
                            Err(e) => {
                                error!(error = %e, "Failed to spawn brain");
                                self.emit(SpurEvent::BrainError {
                                    session: SessionId::new(),
                                    message: e.to_string(),
                                });
                                continue;
                            }
                        }
                    }

                    // ... rest of prompt/streaming logic unchanged ...
                }
```

In the inner streaming loop, update the user input handling to extract text from the enum:

```rust
                    Some(input) = user_input_rx.recv() => {
                        match input {
                            InteractiveInput::Message { text, interrupt } => {
                                if interrupt {
                                    let _ = b.connection.cancel(&b.acp_session_id).await;
                                    cancel_deadline = Some(
                                        tokio::time::Instant::now()
                                            + std::time::Duration::from_secs(5),
                                    );
                                }
                                let msg = if interrupt {
                                    text.strip_prefix('!').unwrap_or(&text).to_string()
                                } else {
                                    text
                                };
                                pending_messages.push_back(msg);
                            }
                            _ => {
                                // Ignore non-message commands during streaming (v1).
                            }
                        }
                    }
```

- [ ] **Step 7: Verify compilation**

Run: `cargo check -p spur-core`
Expected: compiles. May have warnings about unused imports that get cleaned up.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): decompose spawn_brain_session and add session picker handlers"
```

---

### Task 7: Update CLI — `--sessions` flag and forwarding task

**Files:**
- Modify: `crates/spur-cli/src/main.rs:97-103` (Watch command)
- Modify: `crates/spur-cli/src/main.rs:355-393` (Watch handler)

- [ ] **Step 1: Add --sessions flag to Watch command**

In `crates/spur-cli/src/main.rs`, update the `Watch` variant:

```rust
    Watch {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
        /// Start with session picker open
        #[arg(long)]
        sessions: bool,
    },
```

- [ ] **Step 2: Update Watch handler to pass start_in_picker**

Update the `Commands::Watch` match arm (around line 355):

```rust
        Commands::Watch { brain, sessions } => {
```

Update the `run_tui` call:

```rust
            spur_tui::run_tui(event_rx, Some(tui_tx), Some(perm_rx), sessions).await?;
```

- [ ] **Step 3: Update forwarding task for UserInput enum**

Update the forwarding task to handle all UserInput variants:

```rust
            tokio::spawn(async move {
                while let Some(input) = tui_rx.recv().await {
                    let cmd = match input {
                        spur_tui::UserInput::Message { text, interrupt, .. } => {
                            spur_core::InteractiveInput::Message { text, interrupt }
                        }
                        spur_tui::UserInput::ListSessions => {
                            spur_core::InteractiveInput::ListSessions
                        }
                        spur_tui::UserInput::ResumeSession { session_id } => {
                            spur_core::InteractiveInput::ResumeSession { session_id }
                        }
                    };
                    let _ = user_tx.send(cmd).await;
                }
            });
```

- [ ] **Step 4: Verify full compilation**

Run: `cargo check`
Expected: entire workspace compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): add --sessions flag and update forwarding for session picker"
```

---

### Task 8: Manual testing and polish

**Files:**
- Various minor fixes based on testing

- [ ] **Step 1: Test [s] keybinding**

Run `cargo run -p spur-cli -- watch` and press `[s]` on the Dashboard.

Expected: TUI navigates to SessionPicker in loading state. If no agent is configured, an error state should appear. If an agent is available and supports `list_sessions`, sessions should appear.

- [ ] **Step 2: Test --sessions flag**

Run `cargo run -p spur-cli -- watch --sessions`.

Expected: TUI starts directly in SessionPicker loading state instead of Dashboard splash.

- [ ] **Step 3: Test Esc navigation**

From the SessionPicker (any state), press Esc.

Expected: Returns to Dashboard. No crash, no stuck state.

- [ ] **Step 4: Test session selection (if agent available)**

From populated SessionPicker, navigate with arrow keys and press Enter.

Expected: Selected row shows "loading..." indicator. If `load_session` succeeds, TUI transitions to SessionDetailView and history streams in. If it fails, BrainError appears.

- [ ] **Step 5: Test empty state**

Connect to an agent with no saved sessions.

Expected: "No saved sessions found" message with Esc to go back.

- [ ] **Step 6: Fix any issues found during testing**

Apply minimal fixes and commit:

```bash
git add -u
git commit -m "fix(spur-tui): polish session picker after manual testing"
```

- [ ] **Step 7: Final full build check**

Run: `cargo build`
Expected: clean build, no warnings.
