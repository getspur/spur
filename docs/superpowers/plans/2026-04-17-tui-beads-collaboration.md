# TUI-Beads Collaboration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transform the read-only IssuesPanel into an interactive collaboration surface where human and brain agent coordinate work through beads issues, with issue-executor linkage and a DetailPane badge for instant context.

**Architecture:** Interactive IssuesPanel (mirrors AgentsTree pattern) with j/k/Enter selection, `W` key for brain assignment via prompt construction, issue badge in DetailPane header for executor-issue context, quick-keys for status changes, and `/issues` refresh command. All operations flow through the unidirectional TUI → UserInput → Orchestrator → SpurEvent → TUI pipeline.

**Tech Stack:** ratatui 0.29, spur-pm (BeadsAdapter/GitHubAdapter), spur-acp (SpurEventBody), spur-core (Orchestrator, ExecutorLineage), crossterm

**Spec:** `docs/superpowers/specs/2026-04-17-tui-beads-collaboration-design.md`

---

## File Map

### New Files

| File | Responsibility |
|---|---|
| `crates/spur-tui/src/components/issue_detail_pane.rs` | Renders full Issue body/metadata/action-hints when an issue is focused |
| `crates/spur-tui/tests/issues_panel_interaction.rs` | Integration tests for IssuesPanel selection, key routing, IssueFocus state |

### Modified Files

| File | Changes | Phase |
|---|---|---|
| `crates/spur-acp/src/domain/events.rs` | +IssueDetailFetched, +IssueCommandError variants; +issue_id on DelegationRequested | 1, 2 |
| `crates/spur-tui/src/action.rs` | +IssueAction enum, +RefreshIssues, +Issue(IssueAction) variant | 1 |
| `crates/spur-tui/src/app.rs` | +UserInput variants (RefreshIssues, GetIssueDetail, UpdateIssue); process_action handlers | 1, 2 |
| `crates/spur-tui/src/components/issues_panel.rs` | Refactor unit struct → stateful with selection/focus; Phase 2: ◆ linkage | 1, 2 |
| `crates/spur-tui/src/components/mod.rs` | +pub mod issue_detail_pane | 1 |
| `crates/spur-tui/src/views/dashboard.rs` | +Panel::Issues, +IssueFocus enum, Tab cycle, key routing; Phase 2: badge, quick-keys | 1, 2 |
| `crates/spur-tui/src/commands/spur_local.rs` | +/issues command entry | 1 |
| `crates/spur-tui/src/commands/submit_router.rs` | +/work prefix parsing | 1 |
| `crates/spur-core/src/orchestrator.rs` | Handle RefreshIssues, GetIssueDetail; Phase 2: UpdateIssue, issue_id propagation | 1, 2 |
| `crates/spur-core/src/lineage/types.rs` | +issue_id field on ExecutorNode | 2 |
| `crates/spur-core/src/lineage/adapter.rs` | Propagate issue_id from DelegationRequested to ExecutorNode | 2 |
| `crates/spur-core/src/lineage/projection.rs` | +executors_for_issue() method | 2 |
| `crates/spur-tui/src/components/detail_pane.rs` | +issue_badge parameter, right-aligned Title | 2 |
| `crates/spur-cli/src/main.rs` | Map new UserInput variants to InteractiveInput | 1, 2 |
| `crates/spur-pm/src/beads.rs` | Wire since filter in list_issues | 1 |

---

## Phase 1: Interactive Panel + Brain Assignment (MVP)

### Task 1: SpurEventBody — Add IssueDetailFetched and IssueCommandError

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs:156-280`

- [ ] **Step 1: Add IssueDetailFetched variant to SpurEventBody**

Open `crates/spur-acp/src/domain/events.rs`. After the `IssuesLoaded` variant (line ~275), add:

```rust
    /// Response to a TUI request for full issue detail.
    /// Follows SessionsListed / IssuesLoaded precedent for request-response on broadcast.
    IssueDetailFetched {
        /// The ID that was requested — TUI checks against focused issue
        /// to discard stale responses from navigation races.
        requested_id: String,
        /// Full issue data from PmService.
        issue: spur_pm::Issue,
    },

    /// Feedback for a failed issue operation initiated from TUI.
    IssueCommandError {
        operation: String,
        error: String,
    },
```

- [ ] **Step 2: Verify the crate compiles**

Run: `cargo check -p spur-acp`
Expected: PASS (new variants are additive; existing `_ => {}` catch-alls absorb them)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): add IssueDetailFetched + IssueCommandError event variants"
```

---

### Task 2: Action + UserInput — Issue action types

**Files:**
- Modify: `crates/spur-tui/src/action.rs:1-140`
- Modify: `crates/spur-tui/src/app.rs:28-69`

- [ ] **Step 1: Add IssueAction enum and Action variants**

In `crates/spur-tui/src/action.rs`, after the `use` statement at line 1, add the new enum. Then add variants to `Action`:

```rust
/// Issue-related actions dispatched from IssuesPanel or slash commands.
#[derive(Debug, Clone)]
pub enum IssueAction {
    ViewDetail { id: String },
    UpdateStatus { id: String, status: String },
    WorkOn { id: String },
}
```

Add to the `Action` enum (before the `InspectWorkers` variant):

```rust
    /// Refresh the tracked issues list from the PM backend.
    RefreshIssues,
    /// An issue-related action from the IssuesPanel or slash commands.
    Issue(IssueAction),
```

- [ ] **Step 2: Add UserInput variants for issue operations**

In `crates/spur-tui/src/app.rs`, after the `CancelStream` variant (line ~68), add:

```rust
    /// Request the orchestrator to refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Request full issue detail from the PM backend.
    GetIssueDetail {
        id: String,
    },
    /// Update an issue's status/assignee/labels via PM backend.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: PASS (variants added but not yet matched — no exhaustiveness errors since UserInput is non-exhaustive by usage)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): add IssueAction enum + UserInput issue variants"
```

---

### Task 3: IssuesPanel — Refactor to stateful struct with selection

**Files:**
- Modify: `crates/spur-tui/src/components/issues_panel.rs` (full rewrite, 73 → ~130 lines)

- [ ] **Step 1: Rewrite IssuesPanel as a stateful struct**

Replace the entire contents of `crates/spur-tui/src/components/issues_panel.rs`:

```rust
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_pm::IssueSummary;

pub struct IssuesPanel {
    table_state: TableState,
    focused: bool,
}

impl IssuesPanel {
    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
            focused: false,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn select_next(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => (i + 1) % count,
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn select_prev(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(0) | None => count.saturating_sub(1),
            Some(i) => i - 1,
        };
        self.table_state.select(Some(i));
    }

    pub fn selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str> {
        self.table_state
            .selected()
            .and_then(|i| issues.get(i))
            .map(|issue| issue.id.as_str())
    }

    pub fn render(&mut self, issues: &[IssueSummary], frame: &mut Frame, area: Rect) {
        if issues.is_empty() {
            return;
        }

        let border_style = if self.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };

        let title = if self.focused {
            " Issues \u{2500} [j/k] select \u{00b7} [Enter] detail \u{00b7} [W]ork "
        } else {
            " Issues "
        };

        let header = Row::new(["ID", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());

        let rows: Vec<Row> = issues
            .iter()
            .map(|issue| {
                let priority_cell = match issue.priority {
                    Some(0) => Cell::from("P0").fg(Color::Red),
                    Some(1) => Cell::from("P1").fg(Color::Yellow),
                    Some(2) => Cell::from("P2").fg(Color::White),
                    Some(3) => Cell::from("P3").fg(Color::DarkGray),
                    Some(4) => Cell::from("P4").fg(Color::DarkGray),
                    _ => Cell::from("--").fg(Color::DarkGray),
                };

                let status_cell = match issue.status.as_str() {
                    "open" => Cell::from("open").fg(Color::Green),
                    "in_progress" => Cell::from("wip").fg(Color::Cyan),
                    "blocked" => Cell::from("blk").fg(Color::Red),
                    "closed" => Cell::from("done").fg(Color::DarkGray),
                    other => Cell::from(other.to_string()).fg(Color::White),
                };

                Row::new([
                    Cell::from(issue.id.as_str()),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    Cell::from(issue.title.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(8),
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Min(20),
        ];

        let highlight_style = Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD);

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(title).border_style(border_style))
            .highlight_style(highlight_style);

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        (issue_count as u16 + 3).min(max_panel)
    }
}
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p spur-tui`
Expected: Errors in `dashboard.rs` because the old static `IssuesPanel::render(issues, frame, area)` call no longer exists. We fix this in Task 5.

- [ ] **Step 3: Commit (work-in-progress)**

```bash
git add crates/spur-tui/src/components/issues_panel.rs
git commit -m "refactor(spur-tui): IssuesPanel stateful struct with selection"
```

---

### Task 4: IssueDetailPane — New component for full issue rendering

**Files:**
- Create: `crates/spur-tui/src/components/issue_detail_pane.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Create issue_detail_pane.rs**

Create `crates/spur-tui/src/components/issue_detail_pane.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

use spur_pm::Issue;

pub struct IssueDetailPane {
    scroll_offset: u16,
}

impl IssueDetailPane {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn reset(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn render(&self, issue: &Issue, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(Line::from(Span::styled(
            &issue.title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // Metadata row 1: status, priority, type
        let status_style = match issue.status.as_str() {
            "open" => Style::default().fg(Color::Green),
            "in_progress" => Style::default().fg(Color::Cyan),
            "blocked" => Style::default().fg(Color::Red),
            "closed" => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        };
        let pri_str = issue
            .priority
            .map(|p| format!("P{}", p))
            .unwrap_or_else(|| "--".into());
        let type_str = issue.issue_type.as_deref().unwrap_or("--");

        lines.push(Line::from(vec![
            Span::raw("Status: "),
            Span::styled(&issue.status, status_style),
            Span::raw("    Priority: "),
            Span::styled(&pri_str, Style::default().fg(Color::Yellow)),
            Span::raw("    Type: "),
            Span::raw(type_str),
        ]));

        // Metadata row 2: assignee, due
        let assignee = issue.assignee.as_deref().unwrap_or("--");
        let due_str = issue
            .due_at
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "--".into());
        lines.push(Line::from(vec![
            Span::raw("Assignee: "),
            Span::raw(assignee),
            Span::raw("    Due: "),
            Span::raw(&due_str),
        ]));

        // Blocked by
        if !issue.blocked_by.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("Blocked by: "),
                Span::styled(
                    issue.blocked_by.join(", "),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }

        // Labels
        if !issue.labels.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("Labels: "),
                Span::raw(issue.labels.join(", ")),
            ]));
        }

        // Separator
        lines.push(Line::from(
            "\u{2500}".repeat(area.width.saturating_sub(4) as usize),
        ));

        // Body
        if !issue.body.is_empty() {
            for line in issue.body.lines() {
                lines.push(Line::from(line.to_string()));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "(no description)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Footer separator + hints
        lines.push(Line::from(""));
        lines.push(Line::from(
            "\u{2500}".repeat(area.width.saturating_sub(4) as usize),
        ));
        lines.push(Line::from(vec![
            Span::styled("[o]", Style::default().fg(Color::Green)),
            Span::raw("pen "),
            Span::styled("[w]", Style::default().fg(Color::Cyan)),
            Span::raw("ip "),
            Span::styled("[b]", Style::default().fg(Color::Red)),
            Span::raw("locked "),
            Span::styled("[d]", Style::default().fg(Color::DarkGray)),
            Span::raw("one    "),
            Span::styled("[W]", Style::default().fg(Color::Yellow).bold()),
            Span::raw("ork on this    "),
            Span::styled("[Esc]", Style::default().fg(Color::DarkGray)),
            Span::raw(" back"),
        ]));

        let title = format!(" Issue: {} ", &issue.id);
        let block = Block::bordered()
            .title(title)
            .border_style(Style::default().fg(Color::Cyan));

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    }

    /// Render a loading placeholder while the issue is being fetched.
    pub fn render_loading(id: &str, frame: &mut Frame, area: Rect) {
        let title = format!(" Issue: {} ", id);
        let content = Paragraph::new(vec![
            Line::from(""),
            Line::from(""),
            Line::from(Span::styled(
                format!("Loading issue {}...", id),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(Block::bordered().title(title))
        .alignment(ratatui::layout::Alignment::Center);

        frame.render_widget(content, area);
    }
}
```

- [ ] **Step 2: Register module in components/mod.rs**

In `crates/spur-tui/src/components/mod.rs`, add after the existing `pub mod` declarations (around line 10):

```rust
pub mod issue_detail_pane;
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: PASS (new module, not yet used)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/issue_detail_pane.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): add IssueDetailPane component for full issue rendering"
```

---

### Task 5: DashboardView — Panel::Issues, IssueFocus, key routing

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs` (major changes)

This is the largest task. It wires the stateful IssuesPanel into the dashboard with Tab cycling, j/k selection, Enter for detail fetch, W for brain assignment, and event handling.

- [ ] **Step 1: Update imports and Panel enum**

At the top of `crates/spur-tui/src/views/dashboard.rs`, add imports and update the Panel enum.

Replace the existing `Panel` enum (line ~29):

```rust
/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Issues,
    Log,
}
```

Add new imports near the top (after existing use statements):

```rust
use crate::components::issue_detail_pane::IssueDetailPane;
use crate::components::issues_panel::IssuesPanel;
```

Remove the old import line if it exists:
```rust
// REMOVE: use crate::components::issues_panel::IssuesPanel;
// (it was called as a static method before, now it's a struct)
```

- [ ] **Step 2: Add IssueFocus enum and update DashboardView struct**

After the `Panel` enum, add:

```rust
/// State machine for issue detail focus. Invalid states are unrepresentable.
pub enum IssueFocus {
    /// No issue focused. Log area shows ActivityLog or executor DetailPane.
    None,
    /// Issue selected, detail being fetched from backend.
    Loading { id: String },
    /// Full issue loaded. Log area shows IssueDetailPane.
    Loaded { id: String, issue: Box<spur_pm::Issue> },
}
```

Update the `DashboardView` struct fields. Replace `tracked_issues` line and add new fields:

```rust
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    detail_pane: DetailPane,
    issues_panel: IssuesPanel,
    issue_detail_pane: IssueDetailPane,
    issue_focus: IssueFocus,
    input_bar: InputBar,
    focused_panel: Panel,
    focused_node: Option<ExecutorId>,
    verbose: bool,
    text_batch: HashMap<String, (String, Instant)>,
    start_time: Instant,
    tracked_issues: Vec<spur_pm::IssueSummary>,
}
```

Update `DashboardView::new()` to initialize the new fields:

```rust
    pub fn new() -> Self {
        let mut activity_log = ActivityLog::new("Activity");
        activity_log.set_focused(true);

        Self {
            agents_tree: AgentsTree::new(),
            activity_log,
            detail_pane: DetailPane::new(),
            issues_panel: IssuesPanel::new(),
            issue_detail_pane: IssueDetailPane::new(),
            issue_focus: IssueFocus::None,
            input_bar: InputBar::new(),
            focused_panel: Panel::Log,
            focused_node: None,
            verbose: false,
            text_batch: HashMap::new(),
            start_time: Instant::now(),
            tracked_issues: Vec::new(),
        }
    }
```

- [ ] **Step 3: Update Tab cycling in handle_key_inner**

In `handle_key_inner`, find the `KeyCode::Tab` handler (around line 708). Replace it with:

```rust
                KeyCode::Tab => {
                    self.focused_panel = match self.focused_panel {
                        Panel::Agents => {
                            if self.tracked_issues.is_empty() {
                                Panel::Log
                            } else {
                                Panel::Issues
                            }
                        }
                        Panel::Issues => Panel::Log,
                        Panel::Log => Panel::Agents,
                    };
                    self.agents_tree
                        .set_focused(self.focused_panel == Panel::Agents);
                    self.issues_panel
                        .set_focused(self.focused_panel == Panel::Issues);
                    self.activity_log
                        .set_focused(self.focused_panel == Panel::Log);
                    return Some(Action::CycleFocus);
                }
```

- [ ] **Step 4: Add Issues panel key handling**

In `handle_key_inner`, in the Vim Normal mode section (the large `match ch` block around line 481), add a new section BEFORE the existing `'j'` handler for Panel::Issues:

```rust
                    // Issues panel navigation (Vim Normal mode)
                    'j' if self.focused_panel == Panel::Issues => {
                        self.issues_panel.select_next(self.tracked_issues.len());
                        return Some(Action::SelectNext);
                    }
                    'k' if self.focused_panel == Panel::Issues => {
                        self.issues_panel.select_prev(self.tracked_issues.len());
                        return Some(Action::SelectPrev);
                    }
```

Similarly, in the Emacs single-char-nav section (around line 602), add BEFORE the existing 'j'/'k' handlers:

```rust
                    'j' if self.focused_panel == Panel::Issues => {
                        self.input_bar.clear();
                        self.issues_panel.select_next(self.tracked_issues.len());
                        return Some(Action::SelectNext);
                    }
                    'k' if self.focused_panel == Panel::Issues => {
                        self.input_bar.clear();
                        self.issues_panel.select_prev(self.tracked_issues.len());
                        return Some(Action::SelectPrev);
                    }
```

- [ ] **Step 5: Add Enter handler for issue detail + W for WorkOn**

In the Vim Normal block, after the Issues j/k handlers, add:

```rust
                    // Enter on issue → view detail
                    // (handled in the Enter key section below)

                    // W → Work on selected issue (send to brain)
                    'W' if self.focused_panel == Panel::Issues || matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                        let id = match &self.issue_focus {
                            IssueFocus::Loaded { id, .. } => Some(id.clone()),
                            _ => self.issues_panel.selected_id(&self.tracked_issues).map(String::from),
                        };
                        if let Some(id) = id {
                            return Some(Action::Issue(crate::action::IssueAction::WorkOn { id }));
                        }
                        return None;
                    }
```

In the `KeyCode::Enter` handler for empty InputBar (around line 679), update:

```rust
            if key.code == KeyCode::Enter && self.input_bar.is_empty() {
                if self.focused_panel == Panel::Issues {
                    if let Some(id) = self.issues_panel.selected_id(&self.tracked_issues) {
                        self.issue_focus = IssueFocus::Loading { id: id.to_string() };
                        self.issue_detail_pane.reset();
                        return Some(Action::Issue(crate::action::IssueAction::ViewDetail {
                            id: id.to_string(),
                        }));
                    }
                    return None;
                }
                if self.focused_panel == Panel::Agents {
                    return Some(Action::FocusNode);
                }
                return None;
            }
```

- [ ] **Step 6: Add Esc handler for IssueFocus**

In the Esc handler section (around line 719), add a check for issue focus BEFORE the existing `focused_node` check:

```rust
                KeyCode::Esc if !matches!(self.issue_focus, IssueFocus::None) => {
                    self.issue_focus = IssueFocus::None;
                    return Some(Action::UnfocusNode); // reuse existing action for re-render
                }
```

- [ ] **Step 7: Update render_with_lineage to use stateful IssuesPanel + IssueFocus**

In `render_with_lineage`, replace the static IssuesPanel call. Find the block where issues are rendered (around lines 366-372) and replace:

```rust
        if let Some(ic) = issues_chunk {
            self.issues_panel.render(
                &self.tracked_issues,
                frame,
                chunks[ic],
            );
        }
```

For the log area rendering (around lines 374-385), wrap with IssueFocus check:

```rust
        // Log / detail area — depends on issue focus and executor focus
        match &self.issue_focus {
            IssueFocus::Loading { id } => {
                IssueDetailPane::render_loading(id, frame, chunks[log_chunk]);
            }
            IssueFocus::Loaded { issue, .. } => {
                self.issue_detail_pane.render(issue, frame, chunks[log_chunk]);
            }
            IssueFocus::None => {
                match &self.focused_node {
                    Some(id) => {
                        if let Some(node) = lineage.node(id) {
                            self.detail_pane.render(frame, chunks[log_chunk], node);
                        } else {
                            self.activity_log.render(frame, chunks[log_chunk]);
                        }
                    }
                    None => {
                        self.activity_log.render(frame, chunks[log_chunk]);
                    }
                }
            }
        }
```

- [ ] **Step 8: Handle IssueDetailFetched event in handle_spur_event**

In the `View` impl for `DashboardView`, in `handle_spur_event`, add a new arm before the `_ => {}` catch-all:

```rust
            SpurEventBody::IssueDetailFetched { requested_id, issue } => {
                if let IssueFocus::Loading { id } = &self.issue_focus {
                    if id == requested_id {
                        self.issue_focus = IssueFocus::Loaded {
                            id: requested_id.clone(),
                            issue: Box::new(issue.clone()),
                        };
                    }
                }
            }

            SpurEventBody::IssueCommandError { operation, error } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("{} failed: {}", operation, error),
                    kind: LogEntryKind::Error,
                });
                // Clear loading state on error
                if matches!(self.issue_focus, IssueFocus::Loading { .. }) {
                    self.issue_focus = IssueFocus::None;
                }
            }
```

- [ ] **Step 9: Verify compile**

Run: `cargo check -p spur-tui`
Expected: Errors about `IssueDetailPane::render_loading` being a static method vs instance method — fix by checking the implementation. May also see errors about missing `use` for `IssueDetailPane`. Fix any compile errors.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): interactive IssuesPanel with Panel::Issues, IssueFocus, key routing"
```

---

### Task 6: App::process_action — Handle issue actions

**Files:**
- Modify: `crates/spur-tui/src/app.rs:642-850`

- [ ] **Step 1: Add process_action arms for issue actions**

In `App::process_action` (line 642), add new match arms before the final closing brace. Add after the `Action::InspectWorkers` arm:

```rust
            Action::RefreshIssues => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshIssues);
                }
            }

            Action::Issue(issue_action) => {
                match issue_action {
                    crate::action::IssueAction::ViewDetail { id } => {
                        if let Some(ref tx) = self.user_input_tx {
                            let _ = tx.try_send(UserInput::GetIssueDetail { id });
                        }
                    }
                    crate::action::IssueAction::UpdateStatus { id, status } => {
                        if let Some(ref tx) = self.user_input_tx {
                            let _ = tx.try_send(UserInput::UpdateIssue {
                                id,
                                update: spur_pm::IssueUpdate {
                                    status: Some(status),
                                    ..Default::default()
                                },
                            });
                        }
                    }
                    crate::action::IssueAction::WorkOn { id } => {
                        // Construct issue prompt from cached summary
                        let prompt = if let Some(issue) = self.dashboard.tracked_issues().iter().find(|i| i.id == id) {
                            let pri = issue.priority.map(|p| format!("P{}", p)).unwrap_or_default();
                            let itype = issue.issue_type.as_deref().unwrap_or("task");
                            format!(
                                "Work on this issue:\n\n\
                                 Issue: {} \u{2014} {}\n\
                                 Priority: {} | Type: {} | Status: {}\n\n\
                                 Use `get_issue` tool to read full details if needed.\n\
                                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.\n\
                                 Update issue status as you progress.",
                                id, issue.title, pri, itype, issue.status, id,
                            )
                        } else {
                            format!(
                                "Work on issue {}.\n\n\
                                 Use `get_issue` tool to read full details.\n\
                                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.",
                                id, id,
                            )
                        };

                        let blocks = vec![spur_acp::ContentBlock::Text(
                            spur_acp::TextContent::new(prompt),
                        )];

                        if self.session_detail.is_some() {
                            self.process_action(Action::SendMessage {
                                session: spur_acp::SessionId(String::new()),
                                blocks,
                                interrupt: false,
                            });
                        } else {
                            self.process_action(Action::NewSessionWithMessage {
                                blocks,
                                interrupt: false,
                            });
                        }
                    }
                }
            }
```

- [ ] **Step 2: Add tracked_issues accessor to DashboardView**

In `crates/spur-tui/src/views/dashboard.rs`, add a public accessor:

```rust
    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): process_action handlers for issue actions + WorkOn prompt"
```

---

### Task 7: Slash commands — /issues + /work

**Files:**
- Modify: `crates/spur-tui/src/commands/spur_local.rs`
- Modify: `crates/spur-tui/src/commands/submit_router.rs`

- [ ] **Step 1: Add /issues command to SpurLocalSource**

In `crates/spur-tui/src/commands/spur_local.rs`, add to the `entries()` Vec (after the `/vim` entry):

```rust
            CommandEntry {
                name: "issues".into(),
                description: "Refresh issue list from tracker".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::RefreshIssues),
            },
```

- [ ] **Step 2: Add /work prefix parsing in submit_router**

In `crates/spur-tui/src/commands/submit_router.rs`, in the `route()` function, add a prefix check BEFORE the `registry.resolve()` call (around line 55). Insert:

```rust
    // /work <id> → issue WorkOn action
    if let Some(rest) = text.strip_prefix("/work ") {
        let id = rest.trim().to_string();
        if !id.is_empty() {
            return SubmitDecision::Local {
                action: Action::Issue(crate::action::IssueAction::WorkOn { id }),
            };
        }
    }

    // /issue show <id> → issue ViewDetail action
    if let Some(rest) = text.strip_prefix("/issue show ") {
        let id = rest.trim().to_string();
        if !id.is_empty() {
            return SubmitDecision::Local {
                action: Action::Issue(crate::action::IssueAction::ViewDetail { id }),
            };
        }
    }
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/commands/spur_local.rs crates/spur-tui/src/commands/submit_router.rs
git commit -m "feat(spur-tui): /issues refresh + /work <id> slash commands"
```

---

### Task 8: Orchestrator — Handle new UserInput variants

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Add InteractiveInput variants**

In `crates/spur-core/src/orchestrator.rs`, add to the `InteractiveInput` enum (after `CancelStream` at line ~141):

```rust
    /// Refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Fetch full issue detail and emit IssueDetailFetched.
    GetIssueDetail {
        id: String,
    },
    /// Update an issue and emit IssueUpdated.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
```

- [ ] **Step 2: Handle new variants in run_interactive**

In `run_interactive()`, in the `match input {}` block (around line 551), add arms:

```rust
                InteractiveInput::RefreshIssues => {
                    if let Some(pm) = &self.pm_service {
                        match pm.list_issues(spur_pm::IssueFilter {
                            status: Some("open".into()),
                            limit: Some(50),
                            ..Default::default()
                        }).await {
                            Ok(issues) => {
                                let event_issues: Vec<_> = issues.iter().map(|i| {
                                    spur_acp::domain::events::IssueSummaryEvent {
                                        id: i.id.clone(),
                                        source: i.source.to_string(),
                                        title: i.title.clone(),
                                        status: i.status.clone(),
                                        priority: i.priority,
                                        issue_type: i.issue_type.clone(),
                                        assignee: i.assignee.clone(),
                                    }
                                }).collect();
                                self.funnel.emit(SpurEventBody::IssuesLoaded { issues: event_issues });
                            }
                            Err(e) => {
                                self.funnel.emit(SpurEventBody::IssueCommandError {
                                    operation: "refresh_issues".into(),
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                }

                InteractiveInput::GetIssueDetail { id } => {
                    if let Some(pm) = &self.pm_service {
                        match pm.get_issue(&id).await {
                            Ok(issue) => {
                                self.funnel.emit(SpurEventBody::IssueDetailFetched {
                                    requested_id: id,
                                    issue,
                                });
                            }
                            Err(e) => {
                                self.funnel.emit(SpurEventBody::IssueCommandError {
                                    operation: format!("get_issue({})", id),
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                }

                InteractiveInput::UpdateIssue { id, update } => {
                    if let Some(pm) = &self.pm_service {
                        match pm.update_issue(&id, update.clone()).await {
                            Ok(()) => {
                                let status = update.status.unwrap_or_default();
                                self.funnel.emit(SpurEventBody::IssueUpdated {
                                    source: pm.source_str().to_string(),
                                    id,
                                    status,
                                    assignee: update.assignee,
                                });
                            }
                            Err(e) => {
                                self.funnel.emit(SpurEventBody::IssueCommandError {
                                    operation: format!("update_issue({})", id),
                                    error: e.to_string(),
                                });
                            }
                        }
                    }
                }
```

- [ ] **Step 3: Map TUI UserInput to InteractiveInput in CLI**

In `crates/spur-cli/src/main.rs`, in the `match input {}` conversion block (around line 504), add:

```rust
            spur_tui::UserInput::RefreshIssues
                => spur_core::InteractiveInput::RefreshIssues,
            spur_tui::UserInput::GetIssueDetail { id }
                => spur_core::InteractiveInput::GetIssueDetail { id },
            spur_tui::UserInput::UpdateIssue { id, update }
                => spur_core::InteractiveInput::UpdateIssue { id, update },
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p spur-core && cargo check -p spur-cli`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-core): orchestrator handles RefreshIssues, GetIssueDetail, UpdateIssue"
```

---

### Task 9: beads.rs — Wire since filter

**Files:**
- Modify: `crates/spur-pm/src/beads.rs:281-327`

- [ ] **Step 1: Add since filter to list_issues**

In `crates/spur-pm/src/beads.rs`, in the `list_issues` method, after the `assignee` filter arg block (around line 311) and before the `text_search` block, add:

```rust
        if let Some(since) = filter.since {
            args.push("--since".into());
            args.push(since.to_rfc3339());
        }
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p spur-pm`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/beads.rs
git commit -m "fix(spur-pm): wire since filter in BeadsAdapter::list_issues"
```

---

### Task 10: Integration test — IssuesPanel interaction

**Files:**
- Create: `crates/spur-tui/tests/issues_panel_interaction.rs`

- [ ] **Step 1: Write test for IssuesPanel selection and key routing**

Create `crates/spur-tui/tests/issues_panel_interaction.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::{SpurEvent, SpurEventBody};
use spur_core::ExecutorLineage;
use spur_tui::action::{Action, IssueAction};
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx(lineage: &ExecutorLineage) -> spur_tui::views::ViewContext<'_> {
    spur_tui::test_support::test_view_ctx(lineage)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_issues_loaded_event() -> SpurEvent {
    SpurEvent::now(SpurEventBody::IssuesLoaded {
        issues: vec![
            spur_acp::domain::events::IssueSummaryEvent {
                id: "bd-001".into(),
                source: "beads".into(),
                title: "Fix auth timeout".into(),
                status: "open".into(),
                priority: Some(1),
                issue_type: Some("bug".into()),
                assignee: None,
            },
            spur_acp::domain::events::IssueSummaryEvent {
                id: "bd-002".into(),
                source: "beads".into(),
                title: "Update docs".into(),
                status: "open".into(),
                priority: Some(2),
                issue_type: Some("task".into()),
                assignee: None,
            },
        ],
    })
}

#[test]
fn issues_loaded_populates_tracked_issues() {
    let lineage = ExecutorLineage::new();
    let ctx = test_ctx(&lineage);
    let mut dash = DashboardView::new();

    dash.handle_spur_event(&make_issues_loaded_event(), &ctx);

    assert_eq!(dash.tracked_issues().len(), 2);
    assert_eq!(dash.tracked_issues()[0].id, "bd-001"); // P1 sorts before P2
}

#[test]
fn tab_cycles_to_issues_panel_when_issues_exist() {
    let lineage = ExecutorLineage::new();
    let ctx = test_ctx(&lineage);
    let mut dash = DashboardView::new();

    dash.handle_spur_event(&make_issues_loaded_event(), &ctx);

    // Start at Log, Tab → Agents
    let action = dash.handle_key(key(KeyCode::Tab), &ctx);
    assert!(matches!(action, Some(Action::CycleFocus)));

    // Agents → Issues (issues exist)
    let action = dash.handle_key(key(KeyCode::Tab), &ctx);
    assert!(matches!(action, Some(Action::CycleFocus)));

    // Issues → Log
    let action = dash.handle_key(key(KeyCode::Tab), &ctx);
    assert!(matches!(action, Some(Action::CycleFocus)));
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-tui --test issues_panel_interaction`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/tests/issues_panel_interaction.rs
git commit -m "test(spur-tui): integration tests for IssuesPanel interaction"
```

---

## Phase 2: Badge + Linkage + Quick Actions

### Task 11: ExecutorNode — Add issue_id field

**Files:**
- Modify: `crates/spur-core/src/lineage/types.rs:70-114`
- Modify: `crates/spur-acp/src/domain/events.rs:203-218`
- Modify: `crates/spur-core/src/lineage/adapter.rs:88-118`

- [ ] **Step 1: Add issue_id to DelegationRequested event**

In `crates/spur-acp/src/domain/events.rs`, in the `DelegationRequested` variant (line ~203), add after `delegation_plan`:

```rust
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue_id: Option<String>,
```

- [ ] **Step 2: Add issue_id to ExecutorNode**

In `crates/spur-core/src/lineage/types.rs`, add to `ExecutorNode` (after `last_error` around line 108):

```rust
    /// Issue ID linked to this executor via delegation (if any).
    pub issue_id: Option<String>,
```

- [ ] **Step 3: Initialize issue_id in node creation**

In `crates/spur-core/src/lineage/adapter.rs`, in the `DelegationRequested` arm (line ~88), update the match destructuring to include `issue_id`:

```rust
SpurEventBody::DelegationRequested {
    from: _,
    to_agent,
    task,
    request_id: _,
    delegation_plan: _,
    issue_id,
} => {
```

Then after `n.task_spec = task.clone();` (around line 115), add:

```rust
            n.issue_id = issue_id.clone();
```

- [ ] **Step 4: Propagate issue_id in orchestrator event emission**

In `crates/spur-core/src/orchestrator.rs`, find where `DelegationRequested` is emitted (line ~3246). Add `issue_id` to the struct literal. You'll need to extract it from the delegation context — check if `ctx` has an `issue_id` field. If not, add it to the context struct passed to `run_one_worker_attempt`.

The `DelegationRequest` from MCP already has `issue_id: Option<String>`. Find where the orchestrator unpacks `DelegationRequest` and propagates fields to the worker context, then add `issue_id` to the emission:

```rust
funnel.emit(SpurEventBody::DelegationRequested {
    from: ctx.brain_session_id.clone(),
    to_agent: ctx.agent.to_string(),
    task: ctx.task.to_string(),
    request_id: ctx.request_id.to_string(),
    delegation_plan: ctx.delegation_plan.clone(),
    issue_id: ctx.issue_id.clone(),
});
```

- [ ] **Step 5: Verify compile**

Run: `cargo check -p spur-core`

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-core/src/lineage/types.rs crates/spur-core/src/lineage/adapter.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): issue_id on DelegationRequested + ExecutorNode for linkage"
```

---

### Task 12: ExecutorLineage — executors_for_issue() method

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`

- [ ] **Step 1: Add executors_for_issue method**

In `crates/spur-core/src/lineage/projection.rs`, add a public method to `ExecutorLineage`:

```rust
    /// Return nodes linked to the given issue ID.
    pub fn nodes_for_issue(&self, issue_id: &str) -> Vec<&ExecutorNode> {
        self.nodes()
            .filter(|n| n.issue_id.as_deref() == Some(issue_id))
            .collect()
    }
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p spur-core`

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs
git commit -m "feat(spur-core): ExecutorLineage::nodes_for_issue() for issue-executor linkage"
```

---

### Task 13: DetailPane — Issue badge in header

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs:89-100`
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Add issue_badge parameter to DetailPane::render**

In `crates/spur-tui/src/components/detail_pane.rs`, change the `render` method signature (line 89) to:

```rust
    pub fn render(&mut self, frame: &mut Frame, area: Rect, node: &ExecutorNode, issue_badge: Option<&str>) {
```

Update the Block title construction (lines 96-99) to include the badge:

```rust
        let mut block = Block::default()
            .borders(Borders::ALL);

        // Left title: agent name + tabs
        block = block.title(format!(" {} ", node.agent));

        // Right title: issue badge (if linked)
        if let Some(badge) = issue_badge {
            block = block.title(
                ratatui::widgets::block::Title::from(format!(" {} ", badge))
                    .alignment(ratatui::layout::Alignment::Right),
            );
        }

        block = block.title_bottom(following_indicator);
```

- [ ] **Step 2: Update all call sites to pass issue_badge**

In `crates/spur-tui/src/views/dashboard.rs`, in `render_with_lineage`, find where `self.detail_pane.render(frame, chunks[log_chunk], node)` is called (inside the `IssueFocus::None` arm). Update to:

```rust
                        if let Some(node) = lineage.node(id) {
                            let badge = node.issue_id.as_ref().map(|iid| {
                                format_issue_badge(iid, &self.tracked_issues)
                            });
                            self.detail_pane.render(frame, chunks[log_chunk], node, badge.as_deref());
                        } else {
                            self.activity_log.render(frame, chunks[log_chunk]);
                        }
```

Add the `format_issue_badge` helper function to `DashboardView`:

```rust
fn format_issue_badge(issue_id: &str, issues: &[spur_pm::IssueSummary]) -> String {
    let short_id = &issue_id[..8.min(issue_id.len())];
    if let Some(issue) = issues.iter().find(|i| i.id == *issue_id) {
        let pri = issue
            .priority
            .map(|p| format!("P{}", p))
            .unwrap_or_default();
        let max_title = 25;
        let title = if issue.title.len() > max_title {
            let mut end = max_title;
            while !issue.title.is_char_boundary(end) {
                end += 1;
            }
            format!("{}...", &issue.title[..end])
        } else {
            issue.title.clone()
        };
        format!("\u{25c6} {} {} {}", short_id, pri, title)
    } else {
        format!("\u{25c6} {}", short_id)
    }
}
```

- [ ] **Step 3: Add `I` hotkey to open linked issue from DetailPane**

In `crates/spur-tui/src/views/dashboard.rs`, in `handle_key_inner`, in the Vim Normal section, add a handler for `I` when an executor with issue_id is focused:

```rust
                    'I' if self.focused_node.is_some() => {
                        if let Some(ref id) = self.focused_node {
                            if let Some(lineage) = lineage {
                                if let Some(node) = lineage.node(id) {
                                    if let Some(ref iid) = node.issue_id {
                                        self.issue_focus = IssueFocus::Loading { id: iid.clone() };
                                        self.issue_detail_pane.reset();
                                        return Some(Action::Issue(
                                            crate::action::IssueAction::ViewDetail { id: iid.clone() },
                                        ));
                                    }
                                }
                            }
                        }
                        return None;
                    }
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p spur-tui`
Fix any other call sites of `detail_pane.render()` (e.g., in session_detail.rs if it calls DetailPane).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/detail_pane.rs crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): issue badge in DetailPane header + I hotkey for linked issue"
```

---

### Task 14: Quick status keys (o/w/b/d)

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Add quick-key handlers for IssueFocus::Loaded**

In `handle_key_inner`, in the Vim Normal mode section, add handlers for when an issue is loaded. Add BEFORE the existing `'j'` handler for Panel::Agents:

```rust
                    // Quick status keys when issue detail is focused
                    'o' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                        if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                            return Some(Action::Issue(crate::action::IssueAction::UpdateStatus {
                                id: id.clone(),
                                status: "open".into(),
                            }));
                        }
                        return None;
                    }
                    'w' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                        if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                            return Some(Action::Issue(crate::action::IssueAction::UpdateStatus {
                                id: id.clone(),
                                status: "in_progress".into(),
                            }));
                        }
                        return None;
                    }
                    'b' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                        if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                            return Some(Action::Issue(crate::action::IssueAction::UpdateStatus {
                                id: id.clone(),
                                status: "blocked".into(),
                            }));
                        }
                        return None;
                    }
                    'd' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                        if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                            return Some(Action::Issue(crate::action::IssueAction::UpdateStatus {
                                id: id.clone(),
                                status: "closed".into(),
                            }));
                        }
                        return None;
                    }
```

- [ ] **Step 2: Update IssueUpdated handler to refresh IssueFocus**

In the `handle_spur_event` method, in the `IssueUpdated` arm (around line 956), after updating `tracked_issues`, add:

```rust
                // Also update the focused issue detail if it's the same issue
                if let IssueFocus::Loaded { id: ref focus_id, ref mut issue } = self.issue_focus {
                    if focus_id == id {
                        issue.status = status.clone();
                        if let Some(a) = assignee {
                            issue.assignee = Some(a.clone());
                        }
                    }
                }
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): quick status keys o/w/b/d for issue detail view"
```

---

### Task 15: Full build + test verification

- [ ] **Step 1: Run full workspace check**

Run: `cargo check --workspace`
Fix any remaining compile errors across crates.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: All existing tests pass, new test passes.

- [ ] **Step 3: Final commit if any fixes needed**

```bash
git add -A
git commit -m "fix: resolve cross-crate compile issues for TUI-beads collaboration"
```
