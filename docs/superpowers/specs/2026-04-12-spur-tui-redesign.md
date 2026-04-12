# SPUR TUI Redesign: Multi-View Agentic Orchestration Interface

**Date:** 2026-04-12
**Status:** Approved
**Scope:** spur-tui crate (full rewrite)
**PRD Reference:** SPUR_PRD.md sections F1.3, 4 (Architecture), 5 (Data Flow step 9)

## Problem Statement

The current spur-tui is a passive monitoring dashboard (~630 lines) with three issues:

1. **Read-only** — no way to chat with the brain agent, approve permissions, or redirect tasks mid-run.
2. **Single-view** — one screen with flat agent list + raw log. No ReAct trace visualization, no drill-down into sessions.
3. **Noisy log** — every TextDelta token floods the activity log, making it unreadable during fast generation.

## Design Principles

1. **Dashboard for monitoring, Session Detail for interaction.** Two primary screens with distinct purposes.
2. **ACP-first.** Chat messages are ACP `PromptRequest`s to the brain session. Permissions use ACP `request_permission` callbacks. No separate protocol.
3. **Vim-modal.** Normal mode for navigation, Insert mode for chat input. Developers already know this model.
4. **Progressive disclosure.** Dashboard shows condensed ReAct annotations. Session Detail shows full reasoning. Verbose toggle (`v`) on dashboard for users who want everything.

## Architecture

### View Hierarchy

```
Dashboard (home — Esc always returns here)
├── Enter/1-9 → Session Detail (full ReAct trace + chat)
├── p → Issue Queue (PM inbox — Phase 3, not in this spec)
├── c → Cost Detail (analytics — Phase 2, not in this spec)
├── r → Run Dialog (modal overlay — Phase 2)
└── ? → Help (modal overlay)
```

### Phase 1 Scope (this spec)

- Dashboard view (agents tree + activity log)
- Session Detail view (ReAct trace + chat input)
- Permission request handling (inline in Session Detail)
- Component architecture (View trait + Component trait)
- Async event loop (tokio::select! replacing synchronous polling)
- Empty state, help overlay
- Vim-modal keybindings

### Deferred to Later Phases

- Issue Queue view (Phase 3 — PM integration)
- Cost Detail view (Phase 2 — analytics)
- Run Dialog (Phase 2 — start tasks from TUI)
- Log filtering by agent (`/` key)
- Command mode (`:` prefix)

## Views

### Dashboard (home)

Vertical split: agents tree on top (auto-sized, capped), activity log below (fills remaining space), status bar at bottom.

**Agents tree panel:**
- Brain -> worker hierarchy with tree connectors (├─, └─)
- Per-agent line: spinner (if working), name, role badge (BRAIN/WORKER), status, elapsed time, cost
- Capped at max(4 lines, min(40% screen height, 12 lines)). Scrollable if exceeded.
- Color coding: green=working, blue=done, red=error, yellow=rate-limited, gray=idle

**Activity log panel:**
- Shows condensed ReAct annotations: 🧠 (THINK summaries), 🔧 (tool calls), 👁 (observations), → (delegations), ✓/✗ (completions)
- Text deltas batched at 500ms into one-line previews: `▸ ...last 50 chars`
- Tool calls, delegations, and errors show immediately (not batched)
- Sticky-bottom auto-scroll with `▼ following` indicator. Scrolling up pauses follow. Scrolling back to bottom re-enables.
- Press `v` to toggle verbose mode (show all text deltas, no batching)

**Status bar:**
- Left: context-sensitive keybinding hints
- Right: total cost, elapsed time, SPUR branding

**Keybindings (Normal mode):**
- `j/k` or Up/Down: scroll activity log
- `g/G`: jump to top/bottom of log
- `Tab`: cycle focus between agents panel and log panel
- `Enter` or `1-9`: drill into session (→ Session Detail)
- `i`: open input (sends to focused session, or stays on dashboard with input bar)
- `v`: toggle verbose mode
- `r`: run dialog (Phase 2 — no-op for now)
- `p`: issue queue (Phase 3 — no-op for now)
- `c`: cost detail (Phase 2 — no-op for now)
- `?`: help overlay
- `q` or Esc: quit

### Session Detail (drill-down)

Full-screen view of a single brain session's ReAct trace. This is the interactive screen.

**ReAct trace panel (full screen minus input bar and status bar):**
- Full text output from the brain, annotated with ReAct markers:
  - 🧠 THINK — agent reasoning/planning text
  - 🔧 ACT — tool calls with name and arguments
  - 👁 OBSERVE — tool results and agent observations
  - → DELEGATE — delegation events with worker progress inline
  - 💬 YOU — user messages (sent via input bar)
  - ⚠ PERMISSION — permission requests with approval keys
- Text is NOT suppressed in this view — full agent output visible
- Delegation events show inline worker status: task description, spinner, elapsed, completion
- Sticky-bottom scroll (same behavior as dashboard log)

**Input bar (always visible at bottom):**
- Single-line text input with `>` prompt and cursor
- Auto-expands to up to 3 lines for longer messages
- `Enter`: queue message (sent after brain's current turn completes)
- `!` prefix + Enter: interrupt (cancel current turn via ACP session/cancel, then send)
- `Esc`: return to Dashboard (input bar content preserved if non-empty)

**Permission requests (inline):**
- When brain calls ACP `request_permission`, shown inline in the trace:
  ```
  ⚠ PERMISSION: Delete src/old_auth.rs?
    [y]es  [n]o  [a]lways approve deletes
                              auto-approve in 28s
  ```
- `y` = approve once, `n` = deny, `a` = approve all similar operations
- 30-second auto-approve timeout (for unattended runs)
- In headless mode (`spur run --background`), auto-approves immediately

**Header:**
- Session identity: agent name, role, elapsed time, cost
- Breadcrumb: `Dashboard > kiro (brain)`

**Keybindings:**
- Start typing: input goes to input bar (always in insert mode in this view)
- `j/k` or Up/Down: scroll trace (when input bar is empty)
- `g/G`: jump to top/bottom
- `Esc`: return to Dashboard
- `y/n/a`: respond to permission request (when one is active)

### Empty State

When no sessions are active, Dashboard shows a centered welcome:
```
            SPUR
        Multi-agent orchestrator

      No active sessions.

   Press r to run a task
   or from your terminal:
   spur run "fix the auth bug"

   Registered agents:
   ● kiro  ● claude-code  ○ codex
```

### Help Overlay

Modal popup (centered, 60x20) showing all keybindings organized by mode. Dismisses with `Esc` or `?`.

## Data Flow

### Channels

```
Orchestrator                              TUI
    │                                      │
    ├── event_tx (broadcast) ─────────────►│  SpurEvent stream
    │   (agents, log, cost, permissions)   │  (agents, activity, cost)
    │                                      │
    │◄── user_input_tx (mpsc) ────────────┤  User chat messages
    │   (text to send as next prompt)      │  (queued or interrupt)
    │                                      │
    ├── permission_request_tx (mpsc) ─────►│  Permission requests
    │◄── permission_response (oneshot) ────┤  User approval/denial
    │                                      │
```

### New SpurEvent Variants Needed

```rust
SpurEvent::PermissionRequested {
    session: SessionId,
    description: String,
    respond_to: oneshot::Sender<PermissionResponse>,
}

SpurEvent::UserMessage {
    session: SessionId,
    text: String,
}
```

### Async Event Loop

Replace current synchronous `try_recv` + `event::poll` loop with tokio task using `select!`:

```rust
loop {
    tokio::select! {
        // Terminal keyboard/mouse/resize events
        Some(crossterm_event) = crossterm_stream.next() => {
            app.handle_crossterm_event(crossterm_event);
        }
        // SpurEvents from orchestrator
        Ok(spur_event) = event_rx.recv() => {
            app.handle_spur_event(spur_event);
        }
        // Tick for render + spinner animation
        _ = tick_interval.tick() => {
            app.tick();
        }
    }
    // Render after any event
    terminal.draw(|f| app.render(f))?;
    if app.should_quit { break; }
}
```

Requires: `crossterm = { features = ["event-stream"] }` for async event stream.

## Component Architecture

### File Structure

```
spur-tui/src/
  lib.rs              -- pub mod, re-exports run_tui
  app.rs              -- App struct, View enum, event routing, main loop
  action.rs           -- Action enum (message bus between components)
  tui.rs              -- Terminal setup/teardown, async event stream
  views/
    mod.rs            -- View trait definition
    dashboard.rs      -- Dashboard view (agents tree + activity log)
    session_detail.rs -- Session Detail view (ReAct trace + chat)
  components/
    mod.rs            -- Component trait definition
    agents_tree.rs    -- Hierarchical agent list with status
    activity_log.rs   -- Condensed log with sticky-bottom scroll
    react_trace.rs    -- Full ReAct trace renderer
    input_bar.rs      -- Text input with vim-modal behavior
    status_bar.rs     -- Keybindings + cost + elapsed + branding
    help_overlay.rs   -- Modal help popup
```

### View Trait

```rust
pub trait View {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;
    fn handle_spur_event(&mut self, event: &SpurEvent);
    fn render(&self, frame: &mut Frame, area: Rect);
    fn tick(&mut self);  // for animations (spinners)
}
```

### Action Enum

```rust
pub enum Action {
    Quit,
    NavigateTo(ViewId),
    NavigateBack,
    SendMessage { session: SessionId, text: String, interrupt: bool },
    RespondPermission { response: PermissionResponse },
    ToggleVerbose,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ShowHelp,
    HideHelp,
    Tick,
}
```

### App State

```rust
pub struct App {
    current_view: ViewId,
    dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    help_visible: bool,
    // Shared state
    agents: Vec<AgentState>,
    cost_by_agent: HashMap<String, f64>,
    session_agent: HashMap<String, (String, String)>,
    // Channels
    user_input_tx: mpsc::Sender<UserInput>,
}

pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
}
```

## ReAct Trace Annotation

The brain agent's output is annotated with ReAct markers. The TUI detects these patterns in the streaming text:

**Detection heuristic (applied to TextDelta accumulation):**
- Lines starting with common think patterns ("I need to", "Let me", "The issue", "I'll") → 🧠 THINK
- Tool call events from ACP → 🔧 ACT
- Tool result events from ACP → 👁 OBSERVE
- Delegation events from SpurEvent → → DELEGATE
- User messages → 💬 YOU

**Dashboard condensation:** On the dashboard, THINK blocks are truncated to first sentence. ACT shows tool name + key argument. OBSERVE is hidden (tool results are verbose). Delegations show agent + task summary.

**Session Detail:** Full text shown with annotation markers. No truncation.

## Narrow Terminal Support

At terminals narrower than 60 columns:
- Agent role abbreviates: BRAIN→B, WORKER→W
- Status truncates: working→work, rate-limited→ratelim
- Timestamps shorten: 10:32:01→32:01
- Agent prefixes drop role: [brain:kiro]→[kiro]
- Cost in status bar only (not per-agent in narrow mode)

Minimum supported terminal: 40x16.

## Performance Targets (from PRD)

- TUI render frame rate: 30 FPS (tick every 33ms, render on change)
- Memory usage (idle): < 30 MB
- Memory usage (5 active sessions): < 100 MB
- Activity log capped at 5,000 entries (existing cap, maintained)

## Migration from Current Code

The current spur-tui (~630 lines across 3 files) is fully replaced:
- `app.rs` (353 lines) → split into `app.rs` + `views/dashboard.rs` + `views/session_detail.rs`
- `ui.rs` (197 lines) → split into `components/agents_tree.rs` + `components/activity_log.rs` + `components/status_bar.rs`
- `events.rs` (37 lines) → absorbed into `tui.rs` async event stream
- Event processing logic (process_event) reused in DashboardView

## Key Decisions

1. **Vertical split layout** — agents tree on top, log below. Works at all terminal widths (40-200+ cols). Horizontal sidebar eliminated because agent info (name+role+status+time+cost) needs ~50 chars, which doesn't fit in a 24-char sidebar at 80 columns.
2. **Session Detail for chat** — the ReAct trace IS the conversation. User messages appear inline. Input bar is always visible in this view. Dashboard remains read-only monitoring.
3. **Permission requests inline** — shown in the ReAct trace with y/n/a keys. 30s auto-approve timeout for unattended runs.
4. **Condensed vs full** — Dashboard shows condensed ReAct (actions + one-line summaries). Session Detail shows full reasoning. Toggle with `v` on dashboard.
5. **Component pattern** — ratatui-recommended architecture. Each view/component owns its state, handles its events, renders itself. Adding new views (Issues, Cost) = adding new files.
6. **Async event loop** — tokio::select! replacing synchronous polling. Multiplexes crossterm events, SpurEvents, and tick timer.
