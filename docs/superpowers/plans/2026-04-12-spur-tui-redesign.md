# SPUR TUI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the spur-tui crate from a passive monitoring dashboard into a multi-view agentic orchestration interface with Dashboard + Session Detail views, vim-modal chat input, permission handling, and async event loop.

**Architecture:** Component pattern with View trait for screen-level routing and individual component structs for reusable widgets. Async event loop using `tokio::select!` multiplexing crossterm events, SpurEvent broadcast, and tick timer. Two primary views: Dashboard (agents tree + condensed activity log) and Session Detail (full ReAct trace + chat input bar).

**Tech Stack:** Rust, `ratatui` 0.29, `crossterm` 0.28 (with `event-stream` feature), `tokio`, `futures`

**Spec:** `docs/superpowers/specs/2026-04-12-spur-tui-redesign.md`

---

## File Structure

### New files (13 files replacing current 3)
- `crates/spur-tui/src/action.rs` — Action enum (message bus)
- `crates/spur-tui/src/tui.rs` — Terminal setup/teardown, async event stream
- `crates/spur-tui/src/views/mod.rs` — View trait definition
- `crates/spur-tui/src/views/dashboard.rs` — Dashboard view
- `crates/spur-tui/src/views/session_detail.rs` — Session Detail view
- `crates/spur-tui/src/components/mod.rs` — Component trait + shared types
- `crates/spur-tui/src/components/agents_tree.rs` — Agent hierarchy widget
- `crates/spur-tui/src/components/activity_log.rs` — Condensed log with sticky scroll
- `crates/spur-tui/src/components/react_trace.rs` — Full ReAct trace renderer
- `crates/spur-tui/src/components/input_bar.rs` — Text input with cursor
- `crates/spur-tui/src/components/status_bar.rs` — Keybindings + cost + branding
- `crates/spur-tui/src/components/help_overlay.rs` — Modal help popup

### Modified files
- `crates/spur-tui/src/app.rs` — Full rewrite (App struct, view routing, main loop)
- `crates/spur-tui/src/lib.rs` — Update module declarations and re-exports
- `crates/spur-tui/Cargo.toml` — Add `futures`, crossterm `event-stream` feature

### Deleted files
- `crates/spur-tui/src/events.rs` — Absorbed into `tui.rs`
- `crates/spur-tui/src/ui.rs` — Split into components/

---

## Task 1: Update Dependencies and Create Module Skeleton

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`
- Modify: `crates/spur-tui/src/lib.rs`
- Create: `crates/spur-tui/src/action.rs`
- Create: `crates/spur-tui/src/tui.rs`
- Create: `crates/spur-tui/src/views/mod.rs`
- Create: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Update Cargo.toml**

In `crates/spur-tui/Cargo.toml`, change crossterm to include `event-stream` and add `futures`:

```toml
[dependencies]
spur-acp = { workspace = true }
spur-core = { workspace = true }
tokio = { workspace = true }
ratatui = { workspace = true }
crossterm = { workspace = true, features = ["event-stream"] }
anyhow = { workspace = true }
chrono = { workspace = true }
futures = { workspace = true }
```

- [ ] **Step 2: Create `action.rs`**

```rust
use spur_acp::SessionId;

/// Actions that flow between components and the app controller.
#[derive(Debug, Clone)]
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
}

/// Identifies which view is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
}
```

- [ ] **Step 3: Create `views/mod.rs`**

```rust
pub mod dashboard;
pub mod session_detail;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use spur_acp::SpurEvent;

use crate::action::Action;

/// Trait for top-level views (Dashboard, Session Detail, etc.).
pub trait View {
    /// Handle a keyboard event. Return an Action if the view wants the app to do something.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;
    /// Process an orchestrator event, updating internal state.
    fn handle_spur_event(&mut self, event: &SpurEvent);
    /// Render the view into the given frame area.
    fn render(&self, frame: &mut Frame, area: Rect);
    /// Called on each tick (for spinner animations, batched text flush, etc.).
    fn tick(&mut self);
}
```

- [ ] **Step 4: Create `components/mod.rs`**

```rust
pub mod activity_log;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod react_trace;
pub mod status_bar;

use std::collections::HashMap;
use std::time::Instant;

/// Tracked state for a single agent.
#[derive(Debug, Clone)]
pub struct AgentState {
    pub name: String,
    pub role: String,
    pub status: String,
    pub parent: Option<String>,
    pub started_at: Option<Instant>,
    pub cost: f64,
}

/// A single entry in the activity log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub prefix: String,
    pub message: String,
    pub kind: LogEntryKind,
}

/// What kind of log entry this is (for styling and filtering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntryKind {
    Think,
    Act,
    Observe,
    Delegate,
    Complete,
    Error,
    UserMessage,
    Permission,
    Info,
}

/// Maximum log entries before oldest are evicted.
pub const MAX_LOG_ENTRIES: usize = 5_000;
```

- [ ] **Step 5: Create placeholder `tui.rs`**

```rust
//! Terminal setup/teardown and async event stream.
//! Full implementation in Task 9 (App + main loop).
```

- [ ] **Step 6: Update `lib.rs`**

Keep the old modules temporarily so the crate still compiles. Add the new modules:

```rust
pub mod action;
pub mod app;
pub mod components;
pub mod events; // old — will be removed in Task 11
pub mod tui;
pub mod ui; // old — will be removed in Task 11
pub mod views;

pub use app::run_tui;
```

- [ ] **Step 7: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`
Expected: Finished with no errors.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/
git commit -m "scaffold: TUI redesign module skeleton with Action, View trait, Component types"
```

---

## Task 2: Activity Log Component

**Files:**
- Create: `crates/spur-tui/src/components/activity_log.rs`

The activity log is the core widget — used by both Dashboard and Session Detail views.

- [ ] **Step 1: Implement ActivityLog component**

Create `crates/spur-tui/src/components/activity_log.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::{LogEntry, LogEntryKind, MAX_LOG_ENTRIES};

pub struct ActivityLog {
    entries: Vec<LogEntry>,
    scroll_offset: usize,
    is_following: bool,
    title: String,
    focused: bool,
}

impl ActivityLog {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            is_following: true,
            title: title.into(),
            focused: false,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            self.scroll_offset = self.scroll_offset.saturating_sub(drain);
        }
        if self.is_following {
            self.scroll_to_bottom();
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_down(&mut self, visible_height: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
        if self.scroll_offset >= self.entries.len().saturating_sub(visible_height) {
            self.is_following = true;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.entries.len().saturating_sub(1);
        self.is_following = true;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_bottom(following_indicator)
            .borders(Borders::ALL)
            .border_style(border_style);

        let lines: Vec<Line> = self
            .entries
            .iter()
            .map(|entry| {
                let kind_color = match entry.kind {
                    LogEntryKind::Think => Color::DarkGray,
                    LogEntryKind::Act => Color::Yellow,
                    LogEntryKind::Observe => Color::Green,
                    LogEntryKind::Delegate => Color::Cyan,
                    LogEntryKind::Complete => Color::Green,
                    LogEntryKind::Error => Color::Red,
                    LogEntryKind::UserMessage => Color::Yellow,
                    LogEntryKind::Permission => Color::Yellow,
                    LogEntryKind::Info => Color::White,
                };

                Line::from(vec![
                    Span::styled(
                        format!(" {} ", entry.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{} ", entry.prefix),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(&entry.message, Style::default().fg(kind_color)),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset as u16, 0));

        frame.render_widget(paragraph, area);
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}
```

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/activity_log.rs
git commit -m "feat(tui): add ActivityLog component with sticky-bottom scroll"
```

---

## Task 3: Agents Tree Component

**Files:**
- Create: `crates/spur-tui/src/components/agents_tree.rs`

- [ ] **Step 1: Implement AgentsTree component**

Create `crates/spur-tui/src/components/agents_tree.rs`. This renders the brain→worker hierarchy with status, elapsed time, and cost per agent.

Key implementation details:
- Accept `&[AgentState]` in render method
- Build tree structure: agents with `parent: None` are roots (brains), agents with `parent: Some(brain_name)` are children
- Per-agent line: spinner char (cycling braille `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` for working, `○` for idle, `●` for others), name, role badge, status, elapsed, cost
- Tree connectors: `├─` for non-last children, `└─` for last child
- Color coding: green=working/spawned, blue=done, red=error, yellow=rate-limited, gray=idle
- Spinner state: track a `tick_counter: u8` that increments on each `tick()` call, use `SPINNER_CHARS[tick_counter % 10]`
- Elapsed time: format as `Xm Ys` from `AgentState.started_at` if present
- Focused border: cyan when focused, dark gray otherwise

The component should have:
- `pub fn new() -> Self`
- `pub fn set_focused(&mut self, focused: bool)`
- `pub fn tick(&mut self)` — increment spinner counter
- `pub fn render(&self, frame: &mut Frame, area: Rect, agents: &[AgentState])`

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/agents_tree.rs
git commit -m "feat(tui): add AgentsTree component with brain→worker hierarchy"
```

---

## Task 4: Status Bar and Help Overlay Components

**Files:**
- Create: `crates/spur-tui/src/components/status_bar.rs`
- Create: `crates/spur-tui/src/components/help_overlay.rs`

- [ ] **Step 1: Implement StatusBar**

Create `crates/spur-tui/src/components/status_bar.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::action::ViewId;

pub struct StatusBar;

impl StatusBar {
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        view: &ViewId,
        total_cost: f64,
        elapsed: &str,
    ) {
        let hints = match view {
            ViewId::Dashboard => " [i]nput [Enter]session [r]un [c]ost [?]help [q]uit",
            ViewId::SessionDetail(_) => " [Enter]send [Esc]back [j/k]scroll [?]help",
        };

        let line = Line::from(vec![
            Span::styled(
                hints,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::DIM),
            ),
            Span::raw("  "),
            Span::styled(
                format!("${:.2}", total_cost),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(" {} ", elapsed),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "SPUR",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }
}
```

- [ ] **Step 2: Implement HelpOverlay**

Create `crates/spur-tui/src/components/help_overlay.rs`:

```rust
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn render(frame: &mut Frame, area: Rect) {
        // Center a 60x20 popup
        let width = 60u16.min(area.width.saturating_sub(4));
        let height = 20u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        // Clear the background
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let help_text = vec![
            Line::from(Span::styled(
                " Dashboard",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k, Up/Down    Scroll activity log"),
            Line::from("  g / G           Jump to top / bottom"),
            Line::from("  Tab             Cycle panel focus"),
            Line::from("  Enter, 1-9      Drill into session"),
            Line::from("  i               Chat with brain"),
            Line::from("  v               Toggle verbose mode"),
            Line::from("  q, Esc          Quit"),
            Line::from(""),
            Line::from(Span::styled(
                " Session Detail",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  (type)          Input goes to chat bar"),
            Line::from("  Enter           Send message"),
            Line::from("  ! + Enter       Interrupt & send"),
            Line::from("  Esc             Back to Dashboard"),
            Line::from("  y / n / a       Permission: yes/no/always"),
            Line::from(""),
            Line::from(Span::styled(
                " Press ? or Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let paragraph = Paragraph::new(help_text).block(block);
        frame.render_widget(paragraph, popup_area);
    }
}
```

- [ ] **Step 3: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs \
  crates/spur-tui/src/components/help_overlay.rs
git commit -m "feat(tui): add StatusBar and HelpOverlay components"
```

---

## Task 5: Input Bar Component

**Files:**
- Create: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Implement InputBar**

Create `crates/spur-tui/src/components/input_bar.rs`. This is the text input widget for chatting with the brain agent.

Key implementation details:
- Stores input text as `String` and cursor position as `usize`
- Renders as a 1-3 line box with `> ` prompt and cursor indicator (`█`)
- `handle_key()` processes character input, Backspace, Delete, Left/Right arrow, Home/End
- `submit()` extracts the text and clears the buffer. Returns `(text, interrupt)` where interrupt is true if text starts with `!`
- `is_empty()` check for scroll vs input mode decisions
- Height auto-expands based on text length (1 line up to 80 chars, 2 lines up to 160, max 3)

Methods:
- `pub fn new() -> Self`
- `pub fn handle_key(&mut self, key: KeyEvent) -> Option<(String, bool)>` — returns Some((text, interrupt)) on Enter
- `pub fn text(&self) -> &str`
- `pub fn is_empty(&self) -> bool`
- `pub fn clear(&mut self)`
- `pub fn render(&self, frame: &mut Frame, area: Rect)`
- `pub fn required_height(&self) -> u16` — returns 1-3 based on content length

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(tui): add InputBar component for chat input"
```

---

## Task 6: ReAct Trace Component

**Files:**
- Create: `crates/spur-tui/src/components/react_trace.rs`

- [ ] **Step 1: Implement ReactTrace**

Create `crates/spur-tui/src/components/react_trace.rs`. This renders the full ReAct trace for the Session Detail view. Unlike ActivityLog (condensed), this shows full text with ReAct annotations.

Key implementation details:
- Stores trace entries as `Vec<TraceEntry>` where each entry has a `TraceKind` (Think, Act, Observe, Delegate, UserMessage, Permission) and full text content
- Uses the same sticky-bottom scroll behavior as ActivityLog
- Rendering: each entry gets a colored emoji prefix:
  - Think: `🧠 THINK` in gray, followed by full text indented
  - Act: `🔧 ACT` in yellow, with tool name + args
  - Observe: `👁 OBSERVE` in green, with tool result
  - Delegate: `→ DELEGATE` in cyan, with agent + task + inline status
  - UserMessage: `💬 YOU` in yellow, with user text
  - Permission: `⚠ PERMISSION` in yellow, with description + key hints
- Active permission state: when a permission is pending, show `[y]es [n]o [a]lways` with countdown timer
- Delegation inline status: show worker progress within the delegation entry (spinner, elapsed)

Types:
```rust
pub enum TraceKind {
    Think,
    Act { tool: String, args: String },
    Observe,
    Delegate { agent: String, task: String, status: String },
    UserMessage,
    Permission { description: String, pending: bool, countdown: u8 },
}

pub struct TraceEntry {
    pub kind: TraceKind,
    pub text: String,
    pub timestamp: String,
}
```

Methods:
- `pub fn new() -> Self`
- `pub fn push(&mut self, entry: TraceEntry)`
- `pub fn scroll_up(&mut self)` / `scroll_down` / `scroll_to_top` / `scroll_to_bottom`
- `pub fn tick(&mut self)` — update spinner, decrement permission countdown
- `pub fn render(&self, frame: &mut Frame, area: Rect)`

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "feat(tui): add ReactTrace component for ReAct trace visualization"
```

---

## Task 7: Dashboard View

**Files:**
- Create: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Implement DashboardView**

Create `crates/spur-tui/src/views/dashboard.rs`. This composes AgentsTree + ActivityLog + StatusBar into the main monitoring view.

Key implementation details:
- Holds: `agents_tree: AgentsTree`, `activity_log: ActivityLog`, `agents: Vec<AgentState>`, `cost_by_agent: HashMap<String, f64>`, `session_agent: HashMap<String, (String, String)>`, `focused_panel: Panel` (Agents or Log), `verbose: bool`, `text_batch: HashMap<String, (String, Instant)>` (for batching text deltas per session)
- `handle_spur_event()`: port the event processing logic from the current `app.rs` `process_event()` method. Map SpurEvent variants to log entries with appropriate `LogEntryKind`. For TextDelta events: if verbose mode, push immediately; otherwise accumulate in `text_batch` and flush in `tick()` after 500ms.
- `handle_key()`: map keybindings per spec — j/k scroll, g/G jump, Tab cycle focus, Enter/1-9 navigate to SessionDetail, v toggle verbose, ? show help, q quit
- `render()`: vertical layout — agents tree on top (height capped), activity log fills below, status bar at bottom (1 line). Use `Constraint::Min(4)` and `Constraint::Max(agents_height)` for the tree panel.
- `tick()`: call `agents_tree.tick()` for spinner animation, flush any text batches older than 500ms as condensed log entries

Layout constraints:
```rust
let agents_height = (agents.len() as u16 + 2).clamp(4, area.height * 40 / 100).min(12);
let chunks = Layout::vertical([
    Constraint::Length(agents_height),  // agents tree
    Constraint::Min(4),                 // activity log (fills)
    Constraint::Length(1),              // status bar
]).split(area);
```

Empty state: if `self.agents.is_empty()`, render centered welcome message instead of the agents tree + log.

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(tui): add Dashboard view composing agents tree + activity log"
```

---

## Task 8: Session Detail View

**Files:**
- Create: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Implement SessionDetailView**

Create `crates/spur-tui/src/views/session_detail.rs`. This is the interactive screen showing the full ReAct trace + chat input.

Key implementation details:
- Holds: `session_id: SessionId`, `agent_name: String`, `role: String`, `react_trace: ReactTrace`, `input_bar: InputBar`, `cost: f64`, `started_at: Instant`
- `handle_spur_event()`: only process events for this session. Map AgentOutput events to TraceEntry types:
  - TextDelta → TraceEntry::Think (full text, no batching in this view)
  - ToolCallStart → TraceEntry::Act with tool name + input
  - ToolCallResult → TraceEntry::Observe
  - DelegationRequested → TraceEntry::Delegate
  - Error → TraceEntry with error styling
  - Complete → completion entry
- `handle_key()`:
  - When input_bar is non-empty or any key is a printable char: route to input_bar.handle_key()
  - When input_bar is empty: j/k scroll, g/G jump, Esc → NavigateBack
  - When permission is pending: y/n/a keys respond
  - Input bar Enter → Action::SendMessage with session_id, text, interrupt flag
- `render()`: vertical layout — header (1 line: breadcrumb + elapsed + cost), trace (fills), input bar (1-3 lines), status bar (1 line)
- `tick()`: call react_trace.tick()

Header rendering:
```rust
let header = Line::from(vec![
    Span::styled(" Dashboard > ", Style::default().fg(Color::DarkGray)),
    Span::styled(&self.agent_name, Style::default().fg(Color::Cyan).bold()),
    Span::styled(format!(" ({})", self.role), Style::default().fg(Color::DarkGray)),
    Span::raw("  "),
    Span::styled(elapsed, Style::default().fg(Color::DarkGray)),
    Span::raw("  "),
    Span::styled(format!("${:.2}", self.cost), Style::default().fg(Color::Yellow)),
]);
```

Layout:
```rust
let input_height = self.input_bar.required_height();
let chunks = Layout::vertical([
    Constraint::Length(1),              // header
    Constraint::Min(4),                 // react trace (fills)
    Constraint::Length(input_height),   // input bar
    Constraint::Length(1),              // status bar
]).split(area);
```

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(tui): add SessionDetail view with ReAct trace + chat input"
```

---

## Task 9: App Controller and Async Main Loop

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (full rewrite)
- Modify: `crates/spur-tui/src/tui.rs` (full implementation)

- [ ] **Step 1: Implement tui.rs — terminal setup and async event loop**

Replace the placeholder `crates/spur-tui/src/tui.rs` with terminal lifecycle management:

```rust
use std::io;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

pub fn setup() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn teardown(terminal: &mut Tui) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
```

- [ ] **Step 2: Rewrite app.rs — App struct and main loop**

Replace `crates/spur-tui/src/app.rs` entirely. The new App struct:
- Holds the current `ViewId`, `DashboardView`, optional `SessionDetailView`, `help_visible` flag
- `run_tui()` is the async entry point using `tokio::select!`:
  - Arm 1: `crossterm::event::EventStream::next()` for keyboard/resize events
  - Arm 2: `event_rx.recv()` for SpurEvents from orchestrator broadcast
  - Arm 3: `tick_interval.tick()` for 33ms tick (30 FPS)
- After each event: dispatch to active view's handler, process returned Actions, render
- Action handling: Quit sets should_quit, NavigateTo/Back switches views, SendMessage sends to `user_input_tx`, etc.

Key structure:
```rust
pub struct App {
    current_view: ViewId,
    dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    help_visible: bool,
    should_quit: bool,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
}

pub struct UserInput {
    pub session: SessionId,
    pub text: String,
    pub interrupt: bool,
}

pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::new(user_input_tx);
    let tick_rate = Duration::from_millis(33);
    let mut tick_interval = tokio::time::interval(tick_rate);
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

    loop {
        tokio::select! {
            Some(Ok(crossterm_event)) = event_stream.next() => {
                app.handle_crossterm_event(crossterm_event);
            }
            Ok(spur_event) = event_rx.recv() => {
                app.handle_spur_event(spur_event);
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
        }
        terminal.draw(|f| app.render(f))?;
        if app.should_quit { break; }
    }

    tui::teardown(&mut terminal)?;
    Ok(())
}
```

The `handle_crossterm_event` dispatches keyboard events to the active view and processes Actions. Help overlay intercepts `?` and `Esc` before views.

The `handle_spur_event` forwards to ALL views (dashboard always receives events for its log; session_detail only processes events for its session).

The `render` method renders the active view, then overlays help if visible.

- [ ] **Step 3: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/tui.rs
git commit -m "feat(tui): rewrite App controller with async event loop and view routing"
```

---

## Task 10: Wire run_tui into spur-core and spur-cli

**Files:**
- Modify: `crates/spur-tui/src/lib.rs`
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Update lib.rs exports**

Update `crates/spur-tui/src/lib.rs` — remove old modules, export the new entry point:

```rust
pub mod action;
pub mod app;
pub mod components;
pub mod tui;
pub mod views;

pub use app::{run_tui, UserInput};
```

- [ ] **Step 2: Update the `Watch` command in spur-cli**

In `crates/spur-cli/src/main.rs`, the `Commands::Watch` arm currently prints a placeholder. Update it to launch the TUI:

```rust
Commands::Watch => {
    let orch = load_orchestrator(repo_root)?;
    let event_rx = orch.subscribe();
    spur_tui::run_tui(event_rx, None).await?;
    Ok(())
}
```

The `user_input_tx` is `None` for now (Phase 2 will wire it to the orchestrator's multi-turn conversation support).

- [ ] **Step 3: Verify full workspace build**

Run: `source "$HOME/.cargo/env" && cargo check`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/lib.rs crates/spur-cli/src/main.rs
git commit -m "feat(tui): wire new TUI into spur-cli Watch command"
```

---

## Task 11: Delete Old Code

**Files:**
- Delete: `crates/spur-tui/src/events.rs`
- Delete: `crates/spur-tui/src/ui.rs`

- [ ] **Step 1: Delete old files**

```bash
rm crates/spur-tui/src/events.rs crates/spur-tui/src/ui.rs
```

- [ ] **Step 2: Verify build**

Run: `source "$HOME/.cargo/env" && cargo check`
Expected: Clean build. All references to old modules should have been removed in Task 10.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor(tui): delete old events.rs and ui.rs (replaced by views/ and components/)"
```

---

## Task 12: Final Verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `source "$HOME/.cargo/env" && cargo build`
Expected: Finished

- [ ] **Step 2: Run all tests**

Run: `source "$HOME/.cargo/env" && cargo test`
Expected: All pass

- [ ] **Step 3: Run clippy**

Run: `source "$HOME/.cargo/env" && cargo clippy`
Expected: No new errors

- [ ] **Step 4: Verify binary**

Run: `source "$HOME/.cargo/env" && cargo run -p spur-cli -- --help`
Expected: CLI help with `watch` command listed

- [ ] **Step 5: Check file count and structure**

Run: `find crates/spur-tui/src -name '*.rs' | sort`
Expected: 13 files matching the spec's file structure

---

## Summary

| Task | Description | Risk | Files |
|------|-------------|------|-------|
| 1 | Module skeleton + deps + types | Low | 6 files |
| 2 | ActivityLog component | Low | 1 file |
| 3 | AgentsTree component | Medium | 1 file |
| 4 | StatusBar + HelpOverlay | Low | 2 files |
| 5 | InputBar component | Medium | 1 file |
| 6 | ReactTrace component | Medium | 1 file |
| 7 | Dashboard view | **High** | 1 file (largest, composes all) |
| 8 | Session Detail view | **High** | 1 file (interactive, composes all) |
| 9 | App controller + async main loop | **High** | 2 files (wires everything) |
| 10 | Wire into spur-cli | Low | 2 files |
| 11 | Delete old code | Low | delete 2 files |
| 12 | Final verification | Low | none |
