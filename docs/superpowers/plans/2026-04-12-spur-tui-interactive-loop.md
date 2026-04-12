# TUI Interactive Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the feedback loop between the TUI and orchestrator so users can interactively chat with the brain agent through `spur watch`.

**Architecture:** Add `run_interactive()` to the Orchestrator (two-phase loop: stream output + wait for input), wire it from `spur watch` via channels, add InputBar to Dashboard, keep SessionDetailView alive across navigation, and track BrainStatus in App for status indicators.

**Tech Stack:** Rust, tokio (select!, mpsc, oneshot), ratatui, crossterm, agent-client-protocol crate

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `crates/spur-acp/src/domain/events.rs` | SpurEvent enum | Modify: add TurnComplete, BrainError variants |
| `crates/spur-core/src/orchestrator.rs` | Brain-worker pipeline | Modify: extract spawn_brain_session, add run_interactive |
| `crates/spur-tui/src/action.rs` | Action/ViewId enums | No changes needed |
| `crates/spur-tui/src/app.rs` | App state, event loop, view routing | Modify: add BrainStatus, fix SessionDetailView lifecycle |
| `crates/spur-tui/src/components/input_bar.rs` | Text input widget | Modify: add optional status label |
| `crates/spur-tui/src/views/dashboard.rs` | Dashboard view | Modify: add InputBar, wire keyboard routing |
| `crates/spur-tui/src/views/session_detail.rs` | Session Detail view | Modify: add YOU entry on send, set_brain_status |
| `crates/spur-cli/src/main.rs` | CLI entry points | Modify: wire watch command with channels + --brain flag |

---

### Task 1: Add TurnComplete and BrainError to SpurEvent

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 1: Add the two new variants**

In `crates/spur-acp/src/domain/events.rs`, add two new variants to the `SpurEvent` enum after the existing `IssueUpdated` variant:

```rust
    IssueUpdated { source: String, id: String, status: String },
    // ── Interactive loop events ──────────────────────────────────────
    TurnComplete { session: SessionId },
    BrainError { session: SessionId, message: String },
```

- [ ] **Step 2: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-acp 2>&1 | tail -5`
Expected: compiles with no errors (may have warnings from other crates)

- [ ] **Step 3: Handle new variants in Dashboard's handle_spur_event**

In `crates/spur-tui/src/views/dashboard.rs`, the `handle_spur_event` match is exhaustive. Add handlers for the two new variants before the closing `}` of the match, after the `SpurEvent::IssueUpdated` arm:

```rust
            SpurEvent::TurnComplete { session } => {
                let prefix = self.prefix_for_session(&session.0);
                self.set_agent_status_for_session(&session.0, "idle");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::BrainError { session, message } => {
                let prefix = self.prefix_for_session(&session.0);
                self.set_agent_status_for_session(&session.0, "error");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain error: {}", message),
                    kind: LogEntryKind::Error,
                });
            }
```

- [ ] **Step 4: Handle new variants in SessionDetailView's handle_spur_event**

In `crates/spur-tui/src/views/session_detail.rs`, add handlers in the `handle_spur_event` match. Add before the `_ => {}` catch-all:

```rust
            SpurEvent::TurnComplete { session } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Think,
                        text: "Turn complete — ready for input".to_string(),
                        timestamp: Self::now_stamp(),
                    });
                }
            }

            SpurEvent::BrainError { session, message } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Think,
                        text: format!("BRAIN ERROR: {}", message),
                        timestamp: Self::now_stamp(),
                    });
                }
            }
```

- [ ] **Step 5: Verify full workspace compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check 2>&1 | tail -5`
Expected: compiles with no errors

- [ ] **Step 6: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-acp/src/domain/events.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_detail.rs && git commit -m "feat: add TurnComplete and BrainError SpurEvent variants"
```

---

### Task 2: Add BrainStatus enum and tracking to App

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Define BrainStatus enum and add fields to App**

In `crates/spur-tui/src/app.rs`, add the BrainStatus enum after the `UserInput` struct (around line 25):

```rust
/// Tracks the brain agent's current state for status indicators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrainStatus {
    Idle,
    Thinking,
    Streaming,
    Ready,
    Error(String),
}
```

Add two fields to the `App` struct, after `user_input_tx`:

```rust
    brain_status: BrainStatus,
    brain_name: Option<String>,
```

Initialize them in `App::new()`:

```rust
            brain_status: BrainStatus::Idle,
            brain_name: None,
```

- [ ] **Step 2: Update handle_spur_event to track BrainStatus**

In `App::handle_spur_event()`, add brain status tracking. Replace the current body with:

```rust
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        self.dirty = true;

        // Track brain status transitions
        match &event {
            SpurEvent::BrainSpawned { agent, session } => {
                self.brain_status = BrainStatus::Thinking;
                self.brain_name = Some(agent.clone());

                // Always replace SessionDetailView on BrainSpawned
                self.session_detail = Some(SessionDetailView::new(
                    session.clone(),
                    agent.clone(),
                    "brain".to_string(),
                ));

                // Auto-navigate from Dashboard
                if matches!(self.current_view, ViewId::Dashboard) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
            }
            SpurEvent::AgentOutput { session, .. } => {
                // Transition Thinking → Streaming on first output
                if self.brain_status == BrainStatus::Thinking {
                    self.brain_status = BrainStatus::Streaming;
                }
            }
            SpurEvent::TurnComplete { .. } => {
                self.brain_status = BrainStatus::Ready;
            }
            SpurEvent::BrainError { message, .. } => {
                self.brain_status = BrainStatus::Error(message.clone());
            }
            _ => {}
        }

        // Forward to views
        self.dashboard.handle_spur_event(&event);
        if let Some(ref mut detail) = self.session_detail {
            detail.handle_spur_event(&event);
        }
    }
```

- [ ] **Step 3: Update process_action to set Thinking on SendMessage**

In `App::process_action()`, update the `Action::SendMessage` arm to transition brain status:

```rust
            Action::SendMessage {
                session,
                text,
                interrupt,
            } => {
                // Transition to Thinking when sending a message
                if matches!(self.brain_status, BrainStatus::Ready | BrainStatus::Idle | BrainStatus::Error(_)) {
                    self.brain_status = BrainStatus::Thinking;
                }

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput {
                        session,
                        text,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
                }
            }
```

- [ ] **Step 4: Fix NavigateBack — keep SessionDetailView alive**

In `App::process_action()`, change the `NavigateBack` arm to NOT destroy session_detail:

```rust
            Action::NavigateBack => {
                self.current_view = ViewId::Dashboard;
                // Note: session_detail is intentionally kept alive so it
                // continues accumulating events while the Dashboard is shown.
            }
```

- [ ] **Step 5: Simplify NavigateTo(SessionDetail) — just switch view**

In `App::process_action()`, change the `NavigateTo(SessionDetail)` arm to just switch the view instead of recreating:

```rust
            Action::NavigateTo(ViewId::SessionDetail(ref session_id)) => {
                if self.session_detail.is_some() {
                    // Just switch view — don't recreate. BrainSpawned is the only creator.
                    self.current_view = ViewId::SessionDetail(session_id.clone());
                }
                // If no session_detail exists (no brain spawned), ignore.
            }
```

- [ ] **Step 6: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-tui 2>&1 | tail -5`
Expected: compiles (may have unused import warning for `SpurEvent` which we now destructure)

- [ ] **Step 7: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-tui/src/app.rs && git commit -m "feat: add BrainStatus tracking and persistent SessionDetailView"
```

---

### Task 3: Add status label to InputBar

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Add status field and setter**

In `crates/spur-tui/src/components/input_bar.rs`, add a `status` field to the `InputBar` struct:

```rust
pub struct InputBar {
    text: String,
    cursor: usize,
    status: Option<String>,
}
```

Initialize in `new()`:

```rust
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            status: None,
        }
    }
```

Add a setter method after `clear()`:

```rust
    /// Set the status label shown before the prompt (e.g. "[kiro: ready]").
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }
```

- [ ] **Step 2: Update render to show status label**

Replace the `render` method's line-building section. Change the `let line = Line::from(...)` block to:

```rust
        let mut spans = Vec::new();

        // Status label (if set)
        if let Some(ref status) = self.status {
            spans.push(Span::styled(
                format!("{} ", status),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Prompt + text + cursor
        spans.push(Span::raw("> "));
        spans.push(Span::raw(before));
        spans.push(Span::styled("\u{2588}", Style::default().fg(Color::Green)));
        spans.push(Span::raw(after));

        let line = Line::from(spans);
```

Note: `\u{2588}` is the full block character `█`, replacing the string literal for consistency.

- [ ] **Step 3: Update Default impl**

The `Default` impl delegates to `new()`, which now initializes `status: None`. No change needed — verify it still compiles.

- [ ] **Step 4: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-tui 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-tui/src/components/input_bar.rs && git commit -m "feat: add optional status label to InputBar"
```

---

### Task 4: Add InputBar to Dashboard

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Add InputBar field and import**

Add the import at the top of `dashboard.rs`:

```rust
use crate::components::input_bar::InputBar;
```

Add `input_bar` field to `DashboardView`:

```rust
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    input_bar: InputBar,
    agents: Vec<AgentState>,
    // ... rest unchanged
}
```

Initialize in `new()`, after the activity_log creation:

```rust
        Self {
            agents_tree: AgentsTree::new(),
            activity_log,
            input_bar: InputBar::new(),
            agents: Vec::new(),
            // ... rest unchanged
        }
```

- [ ] **Step 2: Add set_brain_status method**

Add a public method to DashboardView:

```rust
    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, name: Option<&str>, status: &str) {
        let label = match (name, status) {
            (_, "idle") => None,
            (Some(n), "thinking") => Some(format!("[{} \u{00b7}\u{00b7}\u{00b7}]", n)),
            (Some(n), "streaming") => Some(format!("[{} \u{25b8}\u{25b8}\u{25b8}]", n)),
            (Some(n), "ready") => Some(format!("[{}: ready]", n)),
            (Some(n), "error") => Some(format!("[{}: error]", n)),
            (None, _) => None,
            (Some(n), other) => Some(format!("[{}: {}]", n, other)),
        };
        self.input_bar.set_status(label);
    }
```

- [ ] **Step 3: Rewrite handle_key to route between InputBar and navigation**

Replace the entire `handle_key` method in the `View for DashboardView` impl:

```rust
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Priority 1: If key is printable or editing, route to InputBar
        let is_editing_key = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
        );

        if is_editing_key {
            // Check if InputBar handles it (Enter on non-empty submits)
            if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
                // Text submitted — send as message
                return Some(Action::SendMessage {
                    session: spur_acp::SessionId::new(),
                    text,
                    interrupt,
                });
            }

            // If InputBar was empty and user typed a navigation char, treat as nav
            if self.input_bar.text().len() == 1 {
                let ch = self.input_bar.text().chars().next().unwrap();
                match ch {
                    'j' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_down(20);
                        return Some(Action::ScrollDown);
                    }
                    'k' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_up();
                        return Some(Action::ScrollUp);
                    }
                    'g' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_to_top();
                        return Some(Action::ScrollToTop);
                    }
                    'G' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_to_bottom();
                        return Some(Action::ScrollToBottom);
                    }
                    'v' => {
                        self.input_bar.clear();
                        self.verbose = !self.verbose;
                        return Some(Action::ToggleVerbose);
                    }
                    'q' => {
                        self.input_bar.clear();
                        return Some(Action::Quit);
                    }
                    '?' => {
                        self.input_bar.clear();
                        return Some(Action::ShowHelp);
                    }
                    _ => {}
                }
            }

            // Enter on empty InputBar → drill into session (if any)
            if key.code == KeyCode::Enter && self.input_bar.is_empty() {
                return self.first_session_id().map(|sid| {
                    Action::NavigateTo(ViewId::SessionDetail(spur_acp::SessionId(sid)))
                });
            }

            return None;
        }

        // Priority 2: Non-editing keys when InputBar is empty
        if self.input_bar.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.activity_log.scroll_up();
                    return Some(Action::ScrollUp);
                }
                KeyCode::Down => {
                    self.activity_log.scroll_down(20);
                    return Some(Action::ScrollDown);
                }
                KeyCode::Tab => {
                    self.focused_panel = match self.focused_panel {
                        Panel::Agents => Panel::Log,
                        Panel::Log => Panel::Agents,
                    };
                    self.agents_tree
                        .set_focused(self.focused_panel == Panel::Agents);
                    self.activity_log
                        .set_focused(self.focused_panel == Panel::Log);
                    return Some(Action::CycleFocus);
                }
                KeyCode::Esc => return Some(Action::Quit),
                _ => {}
            }
        }

        None
    }
```

- [ ] **Step 4: Update render to include InputBar**

In the `render` method, update BOTH the empty-state and normal layout to include the InputBar.

For the **empty state** section (the `if self.agents.is_empty()` block), update the welcome message and add InputBar:

```rust
        if self.agents.is_empty() {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "SPUR",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Multi-agent orchestrator",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Type a task below to start",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center);

            let input_height = self.input_bar.required_height();
            let chunks = Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

            let v_pad = chunks[0].height.saturating_sub(6) / 2;
            let content_area = Rect {
                x: chunks[0].x,
                y: chunks[0].y + v_pad,
                width: chunks[0].width,
                height: chunks[0].height.saturating_sub(v_pad),
            };
            frame.render_widget(paragraph, content_area);
            self.input_bar.render(frame, chunks[1]);
            StatusBar::render(
                frame,
                chunks[2],
                &ViewId::Dashboard,
                self.total_cost(),
                &self.elapsed(),
            );
            return;
        }
```

For the **normal layout**, add InputBar between activity log and status bar:

```rust
        let agents_height = (self.agents.len() as u16 + 2)
            .clamp(4, area.height * 40 / 100)
            .min(12);

        let input_height = self.input_bar.required_height();

        let chunks = Layout::vertical([
            Constraint::Length(agents_height),    // agents tree
            Constraint::Min(4),                  // activity log (fills)
            Constraint::Length(input_height),     // input bar
            Constraint::Length(1),                // status bar
        ])
        .split(area);

        self.agents_tree.render(frame, chunks[0], &self.agents);
        self.activity_log.render(frame, chunks[1]);
        self.input_bar.render(frame, chunks[2]);
        StatusBar::render(
            frame,
            chunks[3],
            &ViewId::Dashboard,
            self.total_cost(),
            &self.elapsed(),
        );
```

- [ ] **Step 5: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-tui 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 6: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-tui/src/views/dashboard.rs && git commit -m "feat: add InputBar to Dashboard for task initiation"
```

---

### Task 5: Wire BrainStatus from App to views

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add set_brain_status to SessionDetailView**

In `crates/spur-tui/src/views/session_detail.rs`, add a `brain_status_label` field and method.

Add field to `SessionDetailView` struct:

```rust
pub struct SessionDetailView {
    session_id: SessionId,
    agent_name: String,
    role: String,
    react_trace: ReactTrace,
    input_bar: InputBar,
    cost: f64,
    started_at: Instant,
}
```

No new field needed — we can use the existing `input_bar.set_status()` directly.

Add a public method to `SessionDetailView` (in the `impl SessionDetailView` block):

```rust
    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, status: &str) {
        let label = match status {
            "idle" => None,
            "thinking" => Some(format!("[{} \u{00b7}\u{00b7}\u{00b7}]", self.agent_name)),
            "streaming" => Some(format!("[{} \u{25b8}\u{25b8}\u{25b8}]", self.agent_name)),
            "ready" => Some(format!("[{}: ready]", self.agent_name)),
            "error" => Some(format!("[{}: error]", self.agent_name)),
            other => Some(format!("[{}: {}]", self.agent_name, other)),
        };
        self.input_bar.set_status(label);
    }
```

- [ ] **Step 2: Push BrainStatus to views from App**

In `crates/spur-tui/src/app.rs`, add a helper method to App that pushes brain_status to both views. Add after the `render` method:

```rust
    /// Push current brain status to both views' InputBars.
    fn sync_brain_status(&mut self) {
        let status_str = match &self.brain_status {
            BrainStatus::Idle => "idle",
            BrainStatus::Thinking => "thinking",
            BrainStatus::Streaming => "streaming",
            BrainStatus::Ready => "ready",
            BrainStatus::Error(_) => "error",
        };

        self.dashboard
            .set_brain_status(self.brain_name.as_deref(), status_str);

        if let Some(ref mut detail) = self.session_detail {
            detail.set_brain_status(status_str);
        }
    }
```

Call `self.sync_brain_status()` at the end of `handle_spur_event()` and at the end of the `SendMessage` arm in `process_action()`:

In `handle_spur_event`, add after the existing views forwarding:

```rust
        // Sync status to InputBars
        self.sync_brain_status();
```

In `process_action` `SendMessage` arm, add at the end:

```rust
                self.sync_brain_status();
```

- [ ] **Step 3: Add YOU entry to SessionDetailView on SendMessage**

In `crates/spur-tui/src/views/session_detail.rs`, add a public method to push a user message to the trace:

```rust
    /// Add a user message to the ReAct trace for instant feedback.
    pub fn push_user_message(&mut self, text: &str) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::UserMessage,
            text: text.to_string(),
            timestamp: Self::now_stamp(),
        });
    }
```

In `crates/spur-tui/src/app.rs`, in `process_action`, update the `SendMessage` arm to also push the user message to the session detail:

After the `self.brain_status = BrainStatus::Thinking;` line, add:

```rust
                // Add user message to Session Detail trace for instant feedback
                if let Some(ref mut detail) = self.session_detail {
                    detail.push_user_message(&text);
                }
```

- [ ] **Step 4: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-tui 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/session_detail.rs && git commit -m "feat: wire BrainStatus to InputBars and add YOU trace entries"
```

---

### Task 6: Extract spawn_brain_session from Orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Define BrainSession struct**

Add the `BrainSession` struct and import `JoinHandle` at the top of `orchestrator.rs`. Add after the `RunResult` struct:

```rust
use tokio::task::JoinHandle;

/// Holds the state of an active brain session.
pub struct BrainSession {
    pub connection: Box<dyn AgentConnection>,
    pub acp_session_id: String,
    pub spur_session_id: SessionId,
    pub brain_name: String,
    pub mcp_server: Arc<McpCallbackServer>,
    pub delegation_handle: JoinHandle<()>,
}
```

- [ ] **Step 2: Extract spawn_brain_session method**

Add a new method to the `Orchestrator` impl. This extracts the brain-spawning logic from `run_adhoc()` lines ~101-226 into a reusable helper:

```rust
    /// Spawn a brain agent session with MCP callback server and delegation handler.
    ///
    /// This is the shared setup used by both `run_adhoc()` and `run_interactive()`.
    pub async fn spawn_brain_session(
        &mut self,
        brain_override: Option<&str>,
    ) -> Result<BrainSession> {
        let session_id = SessionId::new();

        // 1. Resolve brain agent.
        let brain_name = brain_override
            .unwrap_or(&self.config.brain.default)
            .to_string();

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        info!(brain = %brain_name, session = %session_id, "Spawning brain session");
        self.emit(SpurEvent::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        });

        // 2. Start MCP callback server.
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

        // 3. Log session start.
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

        // 4. Spawn brain agent via AgentConnection.
        let mut connection = self.create_connection(&brain_config);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(brain = %brain_name, "Brain agent initialized");

        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        let session_response = connection
            .new_session(self.repo_root.clone(), mcp_servers)
            .await
            .context("Failed to create brain session")?;

        // 5. Spawn delegation handler.
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

- [ ] **Step 3: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-core 2>&1 | tail -10`
Expected: compiles (run_adhoc still uses its own inline version for now)

- [ ] **Step 4: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-core/src/orchestrator.rs && git commit -m "feat: extract spawn_brain_session helper from orchestrator"
```

---

### Task 7: Implement run_interactive on Orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-core/Cargo.toml` (if needed for VecDeque)

- [ ] **Step 1: Add run_interactive method**

Add `use std::collections::VecDeque;` to the imports at the top of `orchestrator.rs`.

Add the `UserInput` import. Since `UserInput` is defined in `spur-tui`, we need to avoid a circular dependency. Instead, define a local struct in `spur-core`:

Add after `BrainSession`:

```rust
/// A user input message from the TUI.
pub struct InteractiveInput {
    pub text: String,
    pub interrupt: bool,
}
```

Add the `run_interactive` method to `Orchestrator`:

```rust
    /// Run an interactive session: multi-turn loop that accepts user input
    /// between brain turns. Used by `spur watch`.
    pub async fn run_interactive(
        mut self,
        mut user_input_rx: mpsc::Receiver<InteractiveInput>,
        brain_override: Option<String>,
    ) -> Result<()> {
        let mut brain: Option<BrainSession> = None;
        let mut pending_messages: VecDeque<String> = VecDeque::new();

        loop {
            // ── Phase 2: Get next message (from queue or user) ──────────
            let text = if let Some(msg) = pending_messages.pop_front() {
                msg
            } else {
                match user_input_rx.recv().await {
                    Some(input) => input.text,
                    None => break, // TUI closed
                }
            };

            // ── Lazy-spawn brain on first message (or after crash) ──────
            if brain.is_none() {
                match self
                    .spawn_brain_session(brain_override.as_deref())
                    .await
                {
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
            let b = brain.as_mut().unwrap();

            // ── Send prompt ─────────────────────────────────────────────
            let prompt_request = PromptRequest::new(
                b.acp_session_id.clone().into(),
                vec![ContentBlock::Text(TextContent::new(text))],
            );

            let mut stream = match b.connection.prompt(prompt_request).await {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Brain prompt failed");
                    self.emit(SpurEvent::BrainError {
                        session: b.spur_session_id.clone(),
                        message: e.to_string(),
                    });
                    // Abort delegation handler, drop brain
                    b.delegation_handle.abort();
                    let _ = b.connection.shutdown().await;
                    brain = None;
                    continue;
                }
            };

            // ── Phase 1: Stream output + check for interrupts ───────────
            let mut cancel_deadline: Option<tokio::time::Instant> = None;

            loop {
                tokio::select! {
                    item = stream.next() => {
                        match item {
                            Some(notification) => {
                                let event = notification_to_session_event(&notification);
                                self.emit(SpurEvent::AgentOutput {
                                    session: b.spur_session_id.clone(),
                                    event,
                                });
                            }
                            None => break, // Turn complete
                        }
                    }
                    Some(input) = user_input_rx.recv() => {
                        if input.interrupt {
                            let _ = b.connection.cancel(&b.acp_session_id).await;
                            cancel_deadline = Some(
                                tokio::time::Instant::now()
                                    + std::time::Duration::from_secs(5),
                            );
                        }
                        pending_messages.push_back(input.text);
                    }
                    _ = async {
                        match cancel_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => futures::future::pending().await,
                        }
                    } => {
                        warn!("Cancel timeout — force-ending stream");
                        break;
                    }
                }
            }

            // Emit turn complete
            self.emit(SpurEvent::TurnComplete {
                session: b.spur_session_id.clone(),
            });
        }

        // ── Cleanup ─────────────────────────────────────────────────────
        if let Some(mut b) = brain.take() {
            b.delegation_handle.abort();
            let _ = b.connection.shutdown().await;
            let _ = b.mcp_server.shutdown();
        }

        info!("Interactive session ended");
        Ok(())
    }
```

- [ ] **Step 2: Export InteractiveInput from spur-core**

In `crates/spur-core/src/lib.rs` (or wherever the crate's public API is), ensure `InteractiveInput` is exported. Check and update:

Run: `cat /Volumes/Projects/spur/crates/spur-core/src/lib.rs` to see current exports, then add `InteractiveInput` to the re-exports.

- [ ] **Step 3: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-core 2>&1 | tail -10`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-core/ && git commit -m "feat: implement run_interactive for multi-turn brain sessions"
```

---

### Task 8: Wire spur watch command

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-cli/Cargo.toml` (add tokio dep if not present)

- [ ] **Step 1: Add --brain flag to Watch command**

In `crates/spur-cli/src/main.rs`, change the `Watch` variant:

```rust
    /// Launch interactive TUI dashboard
    Watch {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
    },
```

- [ ] **Step 2: Wire the watch command with channels and orchestrator**

Replace the `Commands::Watch` match arm:

```rust
        Commands::Watch { brain } => {
            let orch = load_orchestrator(repo_root)?;
            let event_rx = orch.subscribe();

            // Create user input channel
            let (user_tx, user_rx) = tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

            // Spawn interactive orchestrator (moves ownership)
            let orch_handle = tokio::spawn(async move {
                if let Err(e) = orch.run_interactive(user_rx, brain).await {
                    tracing::error!(error = %e, "Interactive session error");
                }
            });

            // Create a wrapper sender that converts TUI's UserInput to InteractiveInput
            let (tui_tx, mut tui_rx) = tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
            tokio::spawn(async move {
                while let Some(input) = tui_rx.recv().await {
                    let _ = user_tx
                        .send(spur_core::InteractiveInput {
                            text: input.text,
                            interrupt: input.interrupt,
                        })
                        .await;
                }
            });

            // Run TUI (blocks main task)
            spur_tui::run_tui(event_rx, Some(tui_tx)).await?;

            // After TUI exits, abort orchestrator
            orch_handle.abort();
            Ok(())
        }
```

- [ ] **Step 3: Verify it compiles**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo check -p spur-cli 2>&1 | tail -10`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
cd /Volumes/Projects/spur && git add crates/spur-cli/src/main.rs && git commit -m "feat: wire spur watch with interactive orchestrator and --brain flag"
```

---

### Task 9: Full build verification and integration test

**Files:**
- No files modified — verification only

- [ ] **Step 1: Full workspace build**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo build 2>&1 | tail -15`
Expected: builds successfully

- [ ] **Step 2: Run any existing tests**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo test 2>&1 | tail -20`
Expected: all existing tests pass

- [ ] **Step 3: Verify spur watch --help shows --brain flag**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo run -p spur-cli -- watch --help 2>&1`
Expected: shows `--brain <BRAIN>` option in help output

- [ ] **Step 4: Verify spur run still works (no regression)**

Run: `source "$HOME/.cargo/env" && cd /Volumes/Projects/spur && cargo run -p spur-cli -- run --help 2>&1`
Expected: shows same help as before (unchanged)

- [ ] **Step 5: Commit any fixes**

If any compilation or test issues were found, fix and commit:

```bash
cd /Volumes/Projects/spur && git add -A && git commit -m "fix: resolve build issues from interactive loop integration"
```
