use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use spur_acp::{DelegationStatus, SessionId, SpurEvent, SpurEventBody};
use spur_core::{ExecutorId, ExecutorLineage};

use crate::action::{Action, ViewId};
use crate::components::activity_log::ActivityLog;
use crate::components::agents_tree::AgentsTree;
use crate::components::detail_pane::{DetailPane, DetailTab};
use crate::components::input_bar::{ActivityKind, EditMode, HandleOutcome, InputBar};

use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;
use crate::components::{LogEntry, LogEntryKind};
use crate::input_history::InputHistoryEntry;

use super::View;

/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Log,
}

/// Explicit input modality for the dashboard.
///
/// Navigate mode: all keys control panels, trees, and overlays. The input bar
/// is visually inactive (gray border). This eliminates the `key_owner()`
/// heuristic — users can see which mode they're in.
///
/// Compose mode: all keys go to the input bar. The input bar is visually
/// active (cyan border). Esc exits to Navigate mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DashboardMode {
    /// Navigation mode — all keys control the view.
    #[default]
    Navigate,
    /// Compose mode — all keys go to the input bar.
    Compose,
}

/// The main dashboard view composing AgentsTree + ActivityLog + StatusBar.
/// All agent-state is now read from `ExecutorLineage` (owned by `App`);
/// this struct only owns the activity log and UI controls.
///
/// Dashboard rendering flows through `View::render`, which delegates to
/// the private `render_with_lineage` helper so the detail pane can access
/// the event-sourced lineage projection via `ViewContext`.
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    detail_pane: DetailPane,
    input_bar: InputBar,
    completion: crate::components::input_completion::InputCompletionPort,
    command_registry: crate::commands::CommandRegistry,
    mention_registry: std::rc::Rc<std::cell::RefCell<crate::mentions::MentionRegistry>>,
    known_worker_names: HashSet<String>,
    cwd: std::path::PathBuf,
    focused_panel: Panel,
    focused_node: Option<ExecutorId>,
    verbose: bool,
    session_attached: bool,
    text_batch: HashMap<String, (String, Instant)>,
    start_time: Instant,
    tracked_issues: Vec<spur_pm::IssueSummary>,
    alert_summary: Option<(usize, usize, usize)>,
    /// When true, agents tree and issues panel are collapsed to maximize
    /// the log / detail viewport.
    layout_zoomed: bool,
    /// Explicit input modality. Replaces the `key_owner()` heuristic with
    /// a visible, predictable state machine.
    mode: DashboardMode,
    /// True when at least one agent is registered. Controls empty-state
    /// rendering: false → setup-nudge, true → example-rich or classic splash.
    agents_configured: bool,
    /// Rotating example prompts shown on the empty Dashboard state.
    example_prompts: Vec<String>,
    /// Current index into `example_prompts`.
    example_index: usize,
    /// When the example prompt last rotated (for 8s auto-advance).
    example_last_rotated: Instant,
    #[cfg(feature = "analytics")]
    live_cost_cache: Option<std::sync::Arc<tokio::sync::RwLock<crate::app::LiveCostCache>>>,
}

/// Convert spur_acp mirror type back to spur_pm::Issue for TUI rendering.
/// Truncate a string to a maximum display length on a UTF-8 boundary,
/// appending `…` if truncation occurred. Used for brain review feedback
/// and other free-form text that could otherwise overflow the TUI log.
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].trim_end().to_string();
    out.push('…');
    out
}

fn format_issue_badge(issue_id: &str, issues: &[spur_pm::IssueSummary]) -> String {
    let short_id: String = issue_id.chars().take(8).collect();
    if let Some(issue) = issues.iter().find(|i| i.id == *issue_id) {
        let pri = issue
            .priority
            .map(|p| format!("P{}", p))
            .unwrap_or_default();
        let max_title = 25;
        let title = if issue.title.len() > max_title {
            let mut end = max_title;
            while end < issue.title.len() && !issue.title.is_char_boundary(end) {
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

impl Default for DashboardView {
    fn default() -> Self {
        Self::new()
    }
}

impl DashboardView {
    pub fn new() -> Self {
        let mut activity_log = ActivityLog::new("Activity");
        activity_log.set_focused(true);

        Self {
            agents_tree: AgentsTree::new(),
            activity_log,
            detail_pane: DetailPane::new(),
            input_bar: InputBar::new(),
            completion: crate::components::input_completion::InputCompletionPort::new(),
            command_registry: crate::commands::CommandRegistry::from_configs(&[]),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(
                crate::mentions::MentionRegistry::new(),
            )),
            known_worker_names: HashSet::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            focused_panel: Panel::Log,
            focused_node: None,
            verbose: false,
            session_attached: false,
            text_batch: HashMap::new(),
            start_time: Instant::now(),
            tracked_issues: Vec::new(),
            alert_summary: None,
            layout_zoomed: false,
            mode: DashboardMode::Navigate,
            agents_configured: true,
            example_prompts: vec![
                "Refactor auth to async/await and benchmark each endpoint".into(),
                "Add input validation to all API handlers".into(),
                "Find the memory leak in the worker pool".into(),
                "Write unit tests for the retry loop".into(),
                "Migrate from serde_json to simd-json".into(),
            ],
            example_index: 0,
            example_last_rotated: Instant::now(),
            #[cfg(feature = "analytics")]
            live_cost_cache: None,
        }
    }

    #[cfg(feature = "analytics")]
    pub fn with_cache(
        cache: std::sync::Arc<tokio::sync::RwLock<crate::app::LiveCostCache>>,
    ) -> Self {
        let mut view = Self::new();
        view.live_cost_cache = Some(cache);
        view
    }

    fn lookup_cached_cost(&self, session_id: &SessionId) -> Option<f64> {
        #[cfg(feature = "analytics")]
        {
            if let Some(cache) = &self.live_cost_cache {
                if let Ok(guard) = cache.try_read() {
                    return guard.by_session.get(session_id).copied();
                }
            }
        }

        #[cfg(not(feature = "analytics"))]
        let _ = session_id;

        None
    }

    /// Returns a cached session aggregate when present, otherwise the first
    /// matching node's current-attempt cost. This is not a lineage aggregate.
    pub fn current_cost(&self, session_id: &SessionId, lineage: &ExecutorLineage) -> Option<f64> {
        if let Some(cost) = self.lookup_cached_cost(session_id) {
            return Some(cost);
        }

        lineage.nodes().find_map(|node| {
            let attempt = node.current_attempt()?;
            if &attempt.session_id == session_id {
                Some(attempt.cost_usd)
            } else {
                None
            }
        })
    }

    fn total_cost(&self, lineage: &ExecutorLineage) -> f64 {
        let mut total = 0.0;
        let mut handled_sessions: HashSet<SessionId> = HashSet::new();

        for node in lineage.nodes() {
            let Some(attempt) = node.current_attempt() else {
                continue;
            };
            let session_id = &attempt.session_id;
            match self.lookup_cached_cost(session_id) {
                Some(cached) => {
                    if handled_sessions.insert(session_id.clone()) {
                        total += cached;
                    }
                }
                None => {
                    total += attempt.cost_usd;
                }
            }
        }

        total
    }

    pub fn set_agents_configured(&mut self, configured: bool) {
        self.agents_configured = configured;
    }

    pub fn agents_configured(&self) -> bool {
        self.agents_configured
    }

    pub fn set_worker_snapshot(&mut self, workers: Vec<crate::mentions::WorkerMentionDescriptor>) {
        self.known_worker_names = workers.iter().map(|d| d.name.clone()).collect();
        self.mention_registry = std::rc::Rc::new(std::cell::RefCell::new(
            crate::mentions::MentionRegistry::for_brain_session(workers),
        ));
    }

    /// Advance rotating example prompts. Call from App::tick().
    pub fn tick(&mut self) {
        if self.example_prompts.len() > 1 {
            let elapsed = self.example_last_rotated.elapsed().as_secs();
            if elapsed >= 8 {
                self.example_index = (self.example_index + 1) % self.example_prompts.len();
                self.example_last_rotated = Instant::now();
            }
        }
    }

    /// Cycle to the next example prompt (e.g. on Tab press).
    pub fn cycle_example(&mut self) {
        if !self.example_prompts.is_empty() {
            self.example_index = (self.example_index + 1) % self.example_prompts.len();
            self.example_last_rotated = Instant::now();
        }
    }

    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }

    /// Build a context-sensitive hint string showing the active panel and
    /// its available key bindings. This makes the invisible `j`/`k` routing
    /// explicit to the user.
    fn panel_context_hint(&self, lineage: &ExecutorLineage) -> String {
        // Mode badge is always visible so users know whether keys navigate or type.
        let mode_badge = match self.mode {
            DashboardMode::Navigate => "[NAV]",
            DashboardMode::Compose => "[INSERT]",
        };

        let body = match &self.focused_node {
            Some(id) => {
                let agent = lineage.node(id).map(|n| n.agent.as_str()).unwrap_or("?");
                format!(
                        "[Detail: {}] h/l/←/→ tabs · Ctrl+1-5 jump · j/k scroll · o toggle · N/P review · Esc back",
                        agent
                    )
            }
            None => match self.focused_panel {
                Panel::Agents => "[Agents] j/k move · Enter focus · c collapse · Tab cycle".into(),
                Panel::Log => {
                    "[Log] j/k scroll · g/G top/bottom · PgUp/PgDn page · Tab cycle".into()
                }
            },
        };

        if self.mode == DashboardMode::Compose {
            format!("{} {} · Esc to navigate", mode_badge, body)
        } else {
            format!("{} {}", mode_badge, body)
        }
    }

    /// Render the one-line hint above the InputBar.
    /// Priority: command/mention hints when typing → panel context when idle.
    fn render_input_hint(
        &self,
        frame: &mut Frame,
        area: Rect,
        input_bar_area: Rect,
        lineage: &ExecutorLineage,
    ) {
        let hint_y = input_bar_area.y.saturating_sub(1);
        if hint_y < area.y {
            return;
        }
        let hint_area = Rect {
            x: input_bar_area.x,
            y: hint_y,
            width: input_bar_area.width,
            height: 1,
        };

        let text = self.input_bar.text();
        let hint = if text.starts_with('/') && !text[1..].contains(char::is_whitespace) {
            // Command hint
            Paragraph::new(Span::styled(
                " Tab to select command \u{00b7} Esc to dismiss",
                Style::default().fg(Color::DarkGray),
            ))
        } else if text.contains('@')
            && !text
                .split('@')
                .next_back()
                .unwrap_or("")
                .contains(char::is_whitespace)
        {
            // Mention hint
            Paragraph::new(Span::styled(
                " Tab to select file \u{00b7} Esc to dismiss",
                Style::default().fg(Color::DarkGray),
            ))
        } else if text.is_empty() {
            // When in Compose mode with an empty input bar, show a typing
            // hint instead of panel navigation keys (those go to the input
            // bar while in Compose mode, so the panel context would mislead).
            let hint_text = if self.mode == DashboardMode::Compose {
                "[INSERT] Typing \u{00b7} Enter to submit \u{00b7} Esc to navigate".to_string()
            } else {
                self.panel_context_hint(lineage)
            };
            Paragraph::new(Span::styled(
                hint_text,
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            return; // No hint needed
        };
        frame.render_widget(hint, hint_area);
    }

    fn now_stamp() -> String {
        crate::components::now_stamp()
    }

    /// Build a short prefix from a session id when no lineage lookup is available.
    fn prefix_for_session(session_id: &str) -> String {
        format!("[{}]", &session_id[..8.min(session_id.len())])
    }

    /// Format elapsed time since TUI start as "Xm Ys".
    fn elapsed(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {:02}s", m, s)
    }

    pub fn agents_tree_mut(&mut self) -> &mut AgentsTree {
        &mut self.agents_tree
    }

    /// Read-only access to the activity log. Intended for tests.
    pub fn activity_log(&self) -> &ActivityLog {
        &self.activity_log
    }

    pub fn set_focused_node(&mut self, id: Option<ExecutorId>) {
        self.focused_node = id;
    }

    pub fn set_focused_panel(&mut self, panel: Panel) {
        self.focused_panel = panel;
    }

    pub fn set_edit_mode(&mut self, mode: EditMode) {
        self.input_bar.set_mode(mode);
    }

    /// Seed the InputBar with global input history (loaded from metadata).
    pub fn seed_input_history(&mut self, entries: Vec<InputHistoryEntry>) {
        self.input_bar.seed_history(entries);
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.mode = DashboardMode::Compose;
        self.input_bar.insert_paste(text);
    }

    pub fn focused_node(&self) -> Option<&ExecutorId> {
        self.focused_node.as_ref()
    }

    pub fn focused_panel(&self) -> Panel {
        self.focused_panel
    }

    pub(crate) fn is_empty_root_input(&self) -> bool {
        self.focused_node.is_none() && self.input_bar.text().is_empty()
    }

    pub fn mode(&self) -> DashboardMode {
        self.mode
    }

    /// Return Dashboard to its root navigation focus.
    pub fn reset_to_root(&mut self) {
        self.mode = DashboardMode::Navigate;
        self.focused_node = None;
        self.focused_panel = Panel::Agents;
        self.agents_tree.set_focused(true);
        self.activity_log.set_focused(false);
        self.completion.reset();
    }

    pub fn detail_pane(&self) -> &DetailPane {
        &self.detail_pane
    }

    pub fn detail_pane_mut(&mut self) -> &mut DetailPane {
        &mut self.detail_pane
    }

    pub fn scroll_activity_up(&mut self) {
        self.activity_log.scroll_up();
    }

    pub fn scroll_activity_down(&mut self) {
        self.activity_log.scroll_down(20);
    }

    pub fn scroll_activity_up_by(&mut self, lines: usize) {
        self.activity_log.scroll_up_by(lines);
    }

    pub fn scroll_activity_down_by(&mut self, lines: usize) {
        self.activity_log.scroll_down_by(lines, 20);
    }

    pub fn scroll_detail_up_by(&mut self, lines: usize) {
        self.detail_pane.scroll_up_by(lines);
    }

    pub fn scroll_detail_down_by(&mut self, lines: usize) {
        self.detail_pane.scroll_down_by(lines);
    }

    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, name: Option<&str>, status: &str, session_attached: bool) {
        self.session_attached = session_attached;
        let mention_count = self.input_bar.protected_ranges().len();
        let mention_suffix = if mention_count > 0 {
            format!(
                " \u{00b7} {} mention{}",
                mention_count,
                if mention_count > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };

        let (label, activity) = match (name, status) {
            (_, "idle") => {
                if mention_count > 0 {
                    (
                        Some(format!(
                            "[{} mention{}]",
                            mention_count,
                            if mention_count > 1 { "s" } else { "" }
                        )),
                        ActivityKind::Idle,
                    )
                } else {
                    (None, ActivityKind::Idle)
                }
            }
            (Some(n), "thinking") => (
                Some(format!("[{} {{spinner}}{}]", n, mention_suffix)),
                ActivityKind::Thinking,
            ),
            (Some(n), "connecting") => (
                Some(format!("[{}: connecting {{spinner}}{}]", n, mention_suffix)),
                ActivityKind::Connecting,
            ),
            (Some(n), "connected") => (
                Some(format!("[{}: connected{}]", n, mention_suffix)),
                ActivityKind::Idle,
            ),
            (Some(n), "streaming") => (
                Some(format!("[{} {{spinner}}{}]", n, mention_suffix)),
                ActivityKind::Streaming,
            ),
            (Some(n), "ready") => (
                Some(format!("[{}: ready{}]", n, mention_suffix)),
                ActivityKind::Idle,
            ),
            (Some(n), "error") => (
                Some(format!("[{}: error{}]", n, mention_suffix)),
                ActivityKind::Idle,
            ),
            (None, _) => {
                if mention_count > 0 {
                    (
                        Some(format!(
                            "[{} mention{}]",
                            mention_count,
                            if mention_count > 1 { "s" } else { "" }
                        )),
                        ActivityKind::Idle,
                    )
                } else {
                    (None, ActivityKind::Idle)
                }
            }
            (Some(n), other) => (
                Some(format!("[{}: {}{}]", n, other, mention_suffix)),
                ActivityKind::Idle,
            ),
        };
        self.input_bar.set_status(label, activity);
    }

    /// True when the InputBar status label is in an animated activity state.
    pub fn input_bar_has_active_animation(&self) -> bool {
        self.input_bar.has_active_animation()
    }

    pub(crate) fn input_bar_active_non_empty(&self) -> bool {
        self.mode == DashboardMode::Compose && !self.input_bar.is_empty()
    }

    pub(crate) fn completion_active(&self) -> bool {
        self.completion.is_active()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn open_slash_picker_for_test(&mut self) {
        self.mode = DashboardMode::Compose;
        self.input_bar.set_text("/".to_string(), 1);
        let env = crate::components::input_completion::CompletionEnv {
            command_registry: &self.command_registry,
            mention_registry: &self.mention_registry,
            cwd: &self.cwd,
            scope: crate::mentions::CompletionScope::PreSession,
            session_config_options: &[],
        };
        self.completion.dispatch(
            crate::components::completion_trigger::IntentEvent::TypedChar('/'),
            &mut self.input_bar,
            &env,
        );
    }

    /// Render the dashboard with access to the current lineage projection.
    pub fn render_with_lineage(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
        ctx: &super::ViewContext,
    ) {
        let lineage = ctx.lineage;
        let license_badge = ctx.license_badge;
        let flag_summary = ctx.flag_summary;
        let tombstone = ctx.tombstone;
        let view_hint_override = ctx.transient_hint_override;
        let node_count = lineage.nodes().count();

        // Compute aggregates once for both empty and non-empty paths.
        let running = lineage
            .nodes()
            .filter(|n| {
                matches!(
                    n.phase,
                    spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning,
                )
            })
            .count();
        let pending_review = lineage.pending_reviews().len();
        let total_cost = self.total_cost(lineage);
        let elapsed = self.elapsed();

        if node_count == 0 {
            if !self.agents_configured {
                self.render_setup_nudge(
                    frame,
                    area,
                    license_badge,
                    flag_summary,
                    tombstone,
                    view_hint_override,
                );
                return;
            }
            self.render_empty_splash(frame, area, lineage, ctx);
            return;
        }

        let input_height = self.input_bar.required_height(area.width);

        let agents_height = if self.layout_zoomed {
            // Zoomed mode: collapse agents to a header bar.
            1u16
        } else {
            (node_count as u16 + 2)
                .clamp(4, area.height * 40 / 100)
                .min(12)
        };

        let constraints = vec![
            Constraint::Length(agents_height), // lineage tree
            Constraint::Min(4),                // activity log / detail (fills)
            Constraint::Length(input_height),  // input bar
            Constraint::Length(1),             // status bar
        ];
        let chunks = Layout::vertical(constraints).split(area);

        let log_chunk = 1;
        let input_chunk = 2;
        let status_chunk = 3;

        if self.layout_zoomed {
            let block = Block::default()
                .title(format!(
                    " Lineage: {} agents · {} running · z restore ",
                    node_count, running
                ))
                .borders(Borders::ALL)
                .border_style(crate::components::focused_border_style(
                    self.focused_panel == Panel::Agents,
                ));
            frame.render_widget(block, chunks[0]);
        } else {
            self.agents_tree.render(frame, chunks[0], lineage);
        }

        match &self.focused_node {
            Some(id) => {
                if let Some(node) = lineage.node(id) {
                    let badge = node
                        .issue_id
                        .as_ref()
                        .map(|iid| format_issue_badge(iid, &self.tracked_issues));
                    let trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.render(
                        frame,
                        chunks[log_chunk],
                        node,
                        badge.as_deref(),
                        trace,
                    );
                } else {
                    self.activity_log.render(frame, chunks[log_chunk]);
                }
            }
            None => {
                self.activity_log.render(frame, chunks[log_chunk]);
            }
        }
        let input_bar_area = chunks[input_chunk];
        self.render_input_hint(frame, area, input_bar_area, lineage);
        self.input_bar
            .set_active(self.mode == DashboardMode::Compose);
        if self.completion.is_active() {
            self.input_bar.render_inert(frame, input_bar_area);
        } else {
            self.input_bar.render(frame, input_bar_area);
        }
        self.completion.render(frame, input_bar_area, area);
        StatusBar::render(
            frame,
            chunks[status_chunk],
            StatusBarProps {
                view: &ViewId::Dashboard,
                tombstone,
                running,
                pending_review,
                total_cost,
                elapsed: &elapsed,
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count: self.tracked_issues.len(),
                alert_summary: self.alert_summary,
                license_badge,
                flag_summary,
                view_hint_override,
            },
        );
    }

    fn render_empty_splash(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: &ExecutorLineage,
        ctx: &super::ViewContext,
    ) {
        let license_badge = ctx.license_badge;
        let flag_summary = ctx.flag_summary;
        let tombstone = ctx.tombstone;
        let view_hint_override = ctx.transient_hint_override;
        let example = self
            .example_prompts
            .get(self.example_index)
            .cloned()
            .unwrap_or_default();
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
                "Type a task below. SPUR breaks it into steps and delegates",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "to specialist agents -- you review before anything merges.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Try asking:",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("-> ", Style::default().fg(Color::DarkGray)),
                Span::styled(example, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "[Tab] cycle examples  ",
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "[s] browse sessions  ",
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled("[?] help", Style::default().fg(Color::DarkGray)),
            ]),
        ];
        let paragraph = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);

        let input_height = self.input_bar.required_height(area.width);
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
        let input_bar_area = chunks[1];
        self.render_input_hint(frame, area, input_bar_area, lineage);
        self.input_bar
            .set_active(self.mode == DashboardMode::Compose);
        if self.completion.is_active() {
            self.input_bar.render_inert(frame, input_bar_area);
        } else {
            self.input_bar.render(frame, input_bar_area);
        }
        self.completion.render(frame, input_bar_area, area);
        StatusBar::render(
            frame,
            chunks[2],
            StatusBarProps {
                view: &ViewId::Dashboard,
                tombstone,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count: self.tracked_issues.len(),
                alert_summary: self.alert_summary,
                license_badge,
                flag_summary,
                view_hint_override,
            },
        );
    }

    fn render_setup_nudge(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        flag_summary: Option<(usize, usize)>,
        tombstone: Option<&Tombstone>,
        view_hint_override: Option<HintOverride<'_>>,
    ) {
        let input_height = self.input_bar.required_height(area.width);
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

        let v_pad = chunks[0].height.saturating_sub(12) / 2;
        let content_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + v_pad,
            width: chunks[0].width,
            height: chunks[0].height.saturating_sub(v_pad),
        };

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
                "Ask for anything -- SPUR breaks it into tasks and delegates",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "to specialist agents, then reviews the results.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "+-------------------------------------------------------------+",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(vec![
                Span::styled("|  ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "! No agents configured. Run this in another terminal:",
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("  |", Style::default().fg(Color::Yellow)),
            ]),
            Line::from(Span::styled(
                "|                                                             |",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(vec![
                Span::styled("|     ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "spur init",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "                                               |",
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(Span::styled(
                "|                                                             |",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(vec![
                Span::styled("|  ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Then restart `spur tui` to begin.",
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    "                          |",
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(Span::styled(
                "+-------------------------------------------------------------+",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Examples of what you can ask (after setup):",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "- Refactor the auth module to async/await and add benchmarks",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "- Find and fix the flaky test in ci/",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "- Add a /health endpoint with proper error handling",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let paragraph = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(paragraph, content_area);

        let input_bar_area = chunks[1];
        self.input_bar.set_active(false);
        if self.completion.is_active() {
            self.input_bar.render_inert(frame, input_bar_area);
        } else {
            self.input_bar.render(frame, input_bar_area);
        }
        self.completion.render(frame, input_bar_area, area);
        StatusBar::render(
            frame,
            chunks[2],
            StatusBarProps {
                view: &ViewId::Dashboard,
                tombstone,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count: self.tracked_issues.len(),
                alert_summary: self.alert_summary,
                license_badge,
                flag_summary,
                view_hint_override,
            },
        );
    }
}

/// Who owns the next keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOwner {
    Composer,
    Picker,
    View,
}

impl DashboardView {
    /// Decide ownership from pre-key state.  No mutation.
    ///
    /// With the modal dashboard, ownership is explicit:
    /// - Navigate mode: nearly everything goes to the view
    /// - Compose mode: nearly everything goes to the composer
    ///
    /// The only exceptions are global bypasses (Ctrl+P/N/O, Alt+a) and
    /// Esc which exits Compose mode.
    fn key_owner(&self, key: KeyEvent) -> KeyOwner {
        if self.completion.is_active() {
            let is_trigger_driven = self.completion.is_trigger_driven();
            let shell_consumes = if is_trigger_driven {
                matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
                ) || ((key.code == KeyCode::Char('c')
                    || key.code == KeyCode::Char('p')
                    || key.code == KeyCode::Char('n'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
            } else {
                true
            };
            if shell_consumes {
                return KeyOwner::Picker;
            }
        }

        // Global bypasses work in both modes.
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            return KeyOwner::View;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(
                key.code,
                KeyCode::Char('p') | KeyCode::Char('n') | KeyCode::Char('o')
            )
        {
            return KeyOwner::View;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('e'))
            && self.is_empty_root_input()
        {
            return KeyOwner::View;
        }
        if key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char('a')) {
            return KeyOwner::View;
        }

        match self.mode {
            DashboardMode::Compose => {
                // Vim Insert/Visual/Operator owns Esc so it can return to
                // Normal mode before the dashboard-level Esc handler runs.
                if key.code == KeyCode::Esc && self.input_bar.wants_esc() {
                    return KeyOwner::Composer;
                }
                // Other Esc presses exit Compose mode and are handled by the view.
                if key.code == KeyCode::Esc {
                    return KeyOwner::View;
                }
                KeyOwner::Composer
            }
            DashboardMode::Navigate => {
                // In Navigate mode, only explicit compose-entry keys go to the composer.
                match key.code {
                    KeyCode::Char(c) if self.input_bar.is_vim_normal() => {
                        // View-reserved keys (panel/detail bindings, Review-tab
                        // labels) win first. Only then does the vim
                        // compose-entry whitelist apply, so plain-`o` on a
                        // focused node hits the observe-toggle binding instead
                        // of being swallowed as vim's "open line below".
                        if self.is_view_action_char(c) {
                            KeyOwner::View
                        } else if matches!(c, 'i' | 'a' | 'A' | 'I' | 'o' | 'O') {
                            KeyOwner::Composer
                        } else {
                            KeyOwner::View
                        }
                    }
                    // Emacs mode: non-view-action characters enter Compose mode.
                    KeyCode::Char(c)
                        if !self.input_bar.is_vim_normal() && !self.is_view_action_char(c) =>
                    {
                        KeyOwner::Composer
                    }
                    _ => KeyOwner::View,
                }
            }
        }
    }

    /// Whether `ch` is a known view-action key in Navigate mode.
    /// These characters navigate instead of entering Compose mode in Emacs.
    fn is_view_action_char(&self, ch: char) -> bool {
        if self.focused_node.is_some() {
            if matches!(ch, 'h' | 'l' | 'o') {
                return true;
            }
            if self.detail_pane.current_tab == DetailTab::Review
                && matches!(ch, 'A' | 'D' | 'M' | 'R')
            {
                return true;
            }
        }
        matches!(
            ch,
            'j' | 'k' | 'g' | 'G' | 'r' | 'v' | '?' | 's' | 'q' | 'z' | 'N' | 'P'
        ) || (ch == 'c' && self.focused_panel == Panel::Agents)
    }

    /// Handle a key that belongs to the view (navigation / actions).
    fn handle_view_key(
        &mut self,
        key: KeyEvent,
        lineage: Option<&ExecutorLineage>,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) -> Option<Action> {
        match key.code {
            KeyCode::Left if self.focused_node.is_some() => {
                self.detail_pane.cycle_tab(false);
                None
            }
            KeyCode::Right if self.focused_node.is_some() => {
                self.detail_pane.cycle_tab(true);
                None
            }
            // ── Vim-style tab cycling (focused node) ──────────────────────
            KeyCode::Char('h')
                if self.focused_node.is_some()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.detail_pane.cycle_tab(false);
                None
            }
            KeyCode::Char('l')
                if self.focused_node.is_some()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.detail_pane.cycle_tab(true);
                None
            }
            // ── Direct tab jumping (focused node, Ctrl modifier) ──────────
            KeyCode::Char('1')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.focused_node.is_some() =>
            {
                self.detail_pane.jump_to_tab(DetailTab::Stream);
                None
            }
            KeyCode::Char('2')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.focused_node.is_some() =>
            {
                self.detail_pane.jump_to_tab(DetailTab::Artifacts);
                None
            }
            KeyCode::Char('3')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.focused_node.is_some() =>
            {
                self.detail_pane.jump_to_tab(DetailTab::Attempts);
                None
            }
            KeyCode::Char('4')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.focused_node.is_some() =>
            {
                self.detail_pane.jump_to_tab(DetailTab::Task);
                None
            }
            KeyCode::Char('5')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.focused_node.is_some() =>
            {
                self.detail_pane.jump_to_tab(DetailTab::Review);
                None
            }
            // ── Toggle observe collapsed (Stream tab) ─────────────────────
            KeyCode::Char('o')
                if self.focused_node.is_some() && key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(ref id) = self.focused_node.clone() {
                    if let Some(trace) = worker_streams.get_mut(&id.0) {
                        trace.toggle_observe_collapsed();
                    }
                }
                None
            }
            KeyCode::Char('o')
                if self.focused_node.is_some()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(ref id) = self.focused_node.clone() {
                    if let Some(trace) = worker_streams.get_mut(&id.0) {
                        trace.toggle_observe_collapsed();
                    }
                }
                None
            }
            // ── View jump: Issue Browser ──────────────────────────────────
            KeyCode::Char('2')
                if self.focused_node.is_none()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::NavigateTo(ViewId::IssueBrowser))
            }
            // ── Half-page scroll ──────────────────────────────────────────
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_down_by(5);
                } else {
                    self.activity_log.scroll_down_by(5, 20);
                }
                Some(Action::ScrollDown)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_up_by(5);
                } else {
                    self.activity_log.scroll_up_by(5);
                }
                Some(Action::ScrollUp)
            }
            // ── Quit ──────────────────────────────────────────────────────
            KeyCode::Char('q')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::Quit)
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.input_bar.is_vim_normal() {
                    // Vim Normal mode view chars (empty bar).
                    if self.focused_node.is_some()
                        && self.detail_pane.current_tab == DetailTab::Review
                    {
                        if let Some(decision) =
                            crate::components::review_card::decision_for_key(ch, None)
                        {
                            if let Some(id) = self.focused_node.clone() {
                                let attempt_n = lineage
                                    .and_then(|l| l.node(&id))
                                    .and_then(|n| n.pending_review.as_ref().map(|r| r.attempt_n))
                                    .unwrap_or(1);
                                return Some(Action::SubmitReview {
                                    executor_id: id.0,
                                    attempt_n,
                                    decision,
                                });
                            }
                        }
                    }
                    let action = match ch {
                        // ── Direct panel jump (Navigate mode) ─────────────────
                        '1' if self.focused_node.is_none() => {
                            self.focused_panel = Panel::Agents;
                            self.agents_tree.set_focused(true);
                            self.activity_log.set_focused(false);
                            None
                        }
                        '3' if self.focused_node.is_none() => {
                            self.focused_panel = Panel::Log;
                            self.agents_tree.set_focused(false);
                            self.activity_log.set_focused(true);
                            None
                        }
                        'j' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            let _ = lineage;
                            Some(Action::SelectNextBy(1))
                        }
                        'j' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let _trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_down();
                            } else {
                                self.activity_log.scroll_down(20);
                            }
                            Some(Action::ScrollDown)
                        }
                        'k' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            let _ = lineage;
                            Some(Action::SelectPrevBy(1))
                        }
                        'k' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let _trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_up();
                            } else {
                                self.activity_log.scroll_up();
                            }
                            Some(Action::ScrollUp)
                        }
                        'r' if !(self.focused_node.is_some()
                            && self.detail_pane.current_tab == DetailTab::Review) =>
                        {
                            Some(Action::JumpToReview)
                        }
                        'N' => Some(Action::JumpToReview),
                        'P' => Some(Action::JumpToPreviousReview),
                        'c' if self.focused_panel == Panel::Agents => Some(Action::ToggleCollapse),
                        'g' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            if let Some(lineage) = lineage {
                                self.agents_tree.select_first(lineage);
                                self.agents_tree.scroll_to_top();
                            }
                            None
                        }
                        'g' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let _trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_to_top();
                            } else {
                                self.activity_log.scroll_to_top();
                            }
                            Some(Action::ScrollToTop)
                        }
                        'G' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            if let Some(lineage) = lineage {
                                self.agents_tree.select_last(lineage);
                                self.agents_tree.scroll_to_bottom();
                            }
                            None
                        }
                        'G' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let _trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_to_bottom();
                            } else {
                                self.activity_log.scroll_to_bottom();
                            }
                            Some(Action::ScrollToBottom)
                        }
                        'v' => {
                            self.verbose = !self.verbose;
                            Some(Action::ToggleVerbose)
                        }
                        'z' => {
                            self.layout_zoomed = !self.layout_zoomed;
                            None
                        }
                        '?' => Some(Action::ShowHelp),
                        's' => Some(Action::RequestSessions),
                        'i' | 'a' | 'A' | 'I' | 'O' => None,
                        _ => return None,
                    };
                    if let Some(a) = action {
                        return Some(a);
                    }
                    return None;
                }

                // Insert mode view chars (empty bar).
                if self.focused_node.is_some() && self.detail_pane.current_tab == DetailTab::Review
                {
                    if let Some(decision) =
                        crate::components::review_card::decision_for_key(ch, None)
                    {
                        if let Some(id) = self.focused_node.clone() {
                            let attempt_n = lineage
                                .and_then(|l| l.node(&id))
                                .and_then(|n| n.pending_review.as_ref().map(|r| r.attempt_n))
                                .unwrap_or(1);
                            return Some(Action::SubmitReview {
                                executor_id: id.0,
                                attempt_n,
                                decision,
                            });
                        }
                    }
                }
                match ch {
                    'j' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        let _ = lineage;
                        Some(Action::SelectNextBy(1))
                    }
                    'j' => {
                        if let Some(ref id) = self.focused_node.clone() {
                            let _trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_down();
                        } else {
                            self.activity_log.scroll_down(20);
                        }
                        Some(Action::ScrollDown)
                    }
                    'k' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        let _ = lineage;
                        Some(Action::SelectPrevBy(1))
                    }
                    'k' => {
                        if let Some(ref id) = self.focused_node.clone() {
                            let _trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_up();
                        } else {
                            self.activity_log.scroll_up();
                        }
                        Some(Action::ScrollUp)
                    }
                    'r' if !(self.focused_node.is_some()
                        && self.detail_pane.current_tab == DetailTab::Review) =>
                    {
                        Some(Action::JumpToReview)
                    }
                    'N' => Some(Action::JumpToReview),
                    'P' => Some(Action::JumpToPreviousReview),
                    'c' if self.focused_panel == Panel::Agents => Some(Action::ToggleCollapse),
                    'g' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        if let Some(lineage) = lineage {
                            self.agents_tree.select_first(lineage);
                            self.agents_tree.scroll_to_top();
                        }
                        None
                    }
                    'g' => {
                        if let Some(ref id) = self.focused_node.clone() {
                            let _trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_to_top();
                        } else {
                            self.activity_log.scroll_to_top();
                        }
                        Some(Action::ScrollToTop)
                    }
                    'G' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        if let Some(lineage) = lineage {
                            self.agents_tree.select_last(lineage);
                            self.agents_tree.scroll_to_bottom();
                        }
                        None
                    }
                    'G' => {
                        if let Some(ref id) = self.focused_node.clone() {
                            let _trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_to_bottom();
                        } else {
                            self.activity_log.scroll_to_bottom();
                        }
                        Some(Action::ScrollToBottom)
                    }
                    'v' => {
                        self.verbose = !self.verbose;
                        Some(Action::ToggleVerbose)
                    }
                    '?' => Some(Action::ShowHelp),
                    's' => Some(Action::RequestSessions),
                    _ => None,
                }
            }
            KeyCode::Up => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_up();
                    Some(Action::ScrollUp)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectPrevBy(1))
                } else {
                    self.activity_log.scroll_up();
                    Some(Action::ScrollUp)
                }
            }
            KeyCode::Down => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_down();
                    Some(Action::ScrollDown)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectNextBy(1))
                } else {
                    self.activity_log.scroll_down(20);
                    Some(Action::ScrollDown)
                }
            }
            // ── Page-wise scroll ──────────────────────────────────────────
            KeyCode::PageUp => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_up_by(10);
                    Some(Action::ScrollUp)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectPrevBy(5))
                } else {
                    self.activity_log.scroll_up_by(10);
                    Some(Action::ScrollUp)
                }
            }
            KeyCode::PageDown => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_down_by(10);
                    Some(Action::ScrollDown)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectNextBy(5))
                } else {
                    self.activity_log.scroll_down_by(10, 20);
                    Some(Action::ScrollDown)
                }
            }
            KeyCode::Char('e')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.is_empty_root_input() =>
            {
                self.cycle_example();
                None
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
                Some(Action::CycleFocus)
            }
            // ── Reverse panel cycle ───────────────────────────────────────
            KeyCode::BackTab => {
                self.focused_panel = match self.focused_panel {
                    Panel::Agents => Panel::Log,
                    Panel::Log => Panel::Agents,
                };
                self.agents_tree
                    .set_focused(self.focused_panel == Panel::Agents);
                self.activity_log
                    .set_focused(self.focused_panel == Panel::Log);
                Some(Action::CycleFocus)
            }
            KeyCode::Esc if self.focused_node.is_some() => Some(Action::UnfocusNode),
            KeyCode::Esc => Some(Action::NavigateBack),
            KeyCode::Enter if self.focused_panel == Panel::Agents => Some(Action::FocusNode),
            _ => None,
        }
    }

    fn handle_key_inner(
        &mut self,
        key: KeyEvent,
        lineage: Option<&ExecutorLineage>,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) -> Option<Action> {
        let key = super::normalize_macos_option(key);

        // Global shortcuts that bypass ownership.
        if matches!(key.code, KeyCode::Char('p')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_prev();
            return None;
        }
        if matches!(key.code, KeyCode::Char('n')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_next();
            return None;
        }
        if matches!(key.code, KeyCode::Char('i')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::ToggleVimMode);
        }

        let owner = self.key_owner(key);

        match owner {
            KeyOwner::Picker => {
                let _ = self.completion.handle_picker_key(key, &mut self.input_bar);
                None
            }
            KeyOwner::Composer => {
                // Enter Compose mode when typing in Navigate mode.
                if self.mode == DashboardMode::Navigate {
                    self.mode = DashboardMode::Compose;
                }
                match self.input_bar.handle_key(key) {
                    HandleOutcome::Submit(text, interrupt) => {
                        self.mode = DashboardMode::Navigate;
                        let env = crate::components::input_completion::CompletionEnv {
                            command_registry: &self.command_registry,
                            mention_registry: &self.mention_registry,
                            cwd: &self.cwd,
                            scope: crate::mentions::CompletionScope::PreSession,
                            session_config_options: &[],
                        };
                        self.completion.dispatch(
                            crate::components::completion_trigger::IntentEvent::Submitted,
                            &mut self.input_bar,
                            &env,
                        );
                        let (captured, ranges, captured_interrupt) = self
                            .input_bar
                            .take_submit_capture()
                            .unwrap_or_else(|| (text, Vec::new(), interrupt));
                        use crate::commands::submit_router::{route, SubmitDecision};
                        match route(
                            &captured,
                            &ranges,
                            &self.command_registry,
                            captured_interrupt,
                        ) {
                            SubmitDecision::Empty => None,
                            SubmitDecision::Send {
                                mut blocks,
                                interrupt,
                            } => {
                                let _ = crate::mentions::hint::prepend_worker_hint(
                                    &mut blocks,
                                    &ranges,
                                    &self.known_worker_names,
                                );
                                if self.session_attached {
                                    Some(Action::SendMessage {
                                        session: spur_acp::SessionId(String::new()),
                                        blocks,
                                        interrupt,
                                    })
                                } else {
                                    Some(Action::NewSessionWithMessage { blocks, interrupt })
                                }
                            }
                            SubmitDecision::Local { action } => Some(action),
                            SubmitDecision::VendorExec { method, params } => {
                                if self.session_attached {
                                    Some(Action::VendorExec {
                                        session: spur_acp::SessionId(String::new()),
                                        method,
                                        params,
                                    })
                                } else {
                                    None
                                }
                            }
                            SubmitDecision::SetSessionConfigOption { config_id, value } => {
                                if self.session_attached {
                                    Some(Action::SetSessionConfigOption { config_id, value })
                                } else {
                                    None
                                }
                            }
                            SubmitDecision::SetSessionModel { value } => {
                                if self.session_attached {
                                    Some(Action::SetSessionModel {
                                        session_id: spur_acp::SessionId(String::new()),
                                        value,
                                    })
                                } else {
                                    None
                                }
                            }
                        }
                    }
                    HandleOutcome::Key(intent) => {
                        let env = crate::components::input_completion::CompletionEnv {
                            command_registry: &self.command_registry,
                            mention_registry: &self.mention_registry,
                            cwd: &self.cwd,
                            scope: crate::mentions::CompletionScope::PreSession,
                            session_config_options: &[],
                        };
                        self.completion.dispatch(intent, &mut self.input_bar, &env);
                        None
                    }
                }
            }
            KeyOwner::View => {
                // Esc in Compose mode exits to Navigate without emitting an action.
                if self.mode == DashboardMode::Compose && key.code == KeyCode::Esc {
                    self.mode = DashboardMode::Navigate;
                    return None;
                }
                self.handle_view_key(key, lineage, worker_streams)
            }
        }
    }
}

impl View for DashboardView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        // NOTE: App bypasses this via handle_key_with_worker_streams to supply
        // the per-executor traces. This fallback uses an empty map (safe but
        // won't route scroll to ReactTrace).
        let mut empty_ws = crate::worker_streams::WorkerStreams::new();
        self.handle_key_inner(key, Some(ctx.lineage), &mut empty_ws)
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
        match &event.body {
            SpurEventBody::BrainConnectStarted { brain } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: "Connecting…".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainConnected { brain } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: "Connected".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainConnectFailed { brain, reason } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: format!("Connect failed: {}", truncate_display(reason, 120)),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::BrainSpawned { agent, session: _ } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", agent),
                    message: "Brain agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerSpawned {
                agent,
                session: _,
                worktree: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[worker:{}]", agent),
                    message: "Worker agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(chunk)
                    | spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                            let trimmed = tc.text.trim();
                            if !trimmed.is_empty() {
                                let entry = self
                                    .text_batch
                                    .entry(session.0.clone())
                                    .or_insert_with(|| (String::new(), Instant::now()));
                                entry.0.push_str(trimmed);
                                if entry.0.len() > 200 {
                                    let mut start = entry.0.len() - 200;
                                    while !entry.0.is_char_boundary(start) {
                                        start += 1;
                                    }
                                    entry.0 = entry.0[start..].to_string();
                                }
                                entry.1 = Instant::now();
                            }
                        }
                    }
                    spur_acp::SessionUpdate::ToolCall(tc) => {
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("\u{1f527} Tool: {}", tc.title),
                            kind: LogEntryKind::Act,
                        });
                    }
                    spur_acp::SessionUpdate::ToolCallUpdate(_) => {
                        // Not logged in dashboard (condensed view)
                    }
                    _ => {
                        // Other variants — no agent-state mutation needed; lineage handles it
                    }
                }
            }

            SpurEventBody::DelegationRequested {
                from: _,
                to_agent,
                task,
                request_id: _,
                delegation_plan: _,
                issue_id: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[brain]".to_string(),
                    message: format!("Delegating to {}: {}", to_agent, task),
                    kind: LogEntryKind::Delegate,
                });
            }

            SpurEventBody::DelegationCompleted {
                worker_session,
                status,
            } => {
                let prefix = Self::prefix_for_session(&worker_session.0);
                let (msg, kind) = match status {
                    DelegationStatus::Success => (
                        "Delegation completed successfully".to_string(),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::Failed { error } => {
                        (format!("Delegation failed: {}", error), LogEntryKind::Error)
                    }
                    DelegationStatus::Conflict { files } => (
                        format!("Delegation conflict in {} files", files.len()),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Timeout => {
                        ("Delegation timed out".to_string(), LogEntryKind::Error)
                    }
                    DelegationStatus::Rejected { reason } => (
                        format!("Delegation rejected: {}", reason),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Modified { reviewer_note } => (
                        format!("Delegation modified: {}", reviewer_note),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::TimedOut {
                        waited_for,
                        fallback,
                    } => (
                        format!(
                            "Delegation review timed out after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        ),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Cancelled { reason } => (
                        format!("Delegation cancelled: {}", reason),
                        LogEntryKind::Error,
                    ),
                    _ => {
                        tracing::warn!("unknown DelegationStatus variant in dashboard activity log — update needed");
                        ("Delegation completed".to_string(), LogEntryKind::Error)
                    }
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg,
                    kind,
                });
            }

            SpurEventBody::SessionCompleted { session, success } => {
                let prefix = Self::prefix_for_session(&session.0);
                let msg = if *success {
                    "Session completed successfully"
                } else {
                    "Session failed"
                };
                let kind = if *success {
                    LogEntryKind::Complete
                } else {
                    LogEntryKind::Error
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg.to_string(),
                    kind,
                });
            }

            SpurEventBody::BrainRetired { session, reason } => {
                // Pair the earlier "Brain agent spawned" entry with an
                // explicit retirement line so the activity log does not
                // show a dangling spawn after `/clear` or session switch.
                let prefix = Self::prefix_for_session(&session.0);
                let reason_label = match reason {
                    spur_acp::domain::events::BrainRetireReason::UserClear => "cleared",
                    spur_acp::domain::events::BrainRetireReason::ResumeSwitch => "switched",
                    spur_acp::domain::events::BrainRetireReason::Shutdown => "shutdown",
                    _ => "retired",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain session retired ({})", reason_label),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::RateLimitDetected { agent, retry_after } => {
                let msg = match retry_after {
                    Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                    None => "Rate limited".to_string(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[{}]", agent),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainFailover { from, to } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("Brain failover: {} -> {}", from, to),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::CostUpdate { .. } => {
                // Cost is now read from lineage.nodes().current_attempt().cost_usd
            }

            SpurEventBody::ConflictDetected { files } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!(
                        "Conflict detected in {} file(s): {}",
                        files.len(),
                        files
                            .iter()
                            .map(|f| f.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssueReceived { source, id } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue received from {}: {}", source, id),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::PrCreated { url } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("PR created: {}", url),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssueUpdated {
                source,
                id,
                status,
                assignee,
            } => {
                if let Some(issue) = self.tracked_issues.iter_mut().find(|i| i.id == *id) {
                    if let Some(ref s) = status {
                        issue.status = s.clone();
                    }
                    if let Some(a) = assignee {
                        issue.assignee = Some(a.clone());
                    }
                }
                let status_suffix = status
                    .as_ref()
                    .map(|s| format!(": {}", s))
                    .unwrap_or_default();
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue {} ({}) updated{}", id, source, status_suffix),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssuesLoaded { issues } => {
                self.tracked_issues = issues
                    .iter()
                    .map(|i| spur_pm::IssueSummary {
                        id: i.id.clone(),
                        source: match i.source.as_str() {
                            "github" => spur_pm::PmSource::GitHub,
                            "linear" => spur_pm::PmSource::Linear,
                            "plane" => spur_pm::PmSource::Plane,
                            _ => spur_pm::PmSource::Beads,
                        },
                        title: i.title.clone(),
                        status: i.status.clone(),
                        labels: Vec::new(),
                        url: String::new(),
                        priority: i.priority,
                        issue_type: i.issue_type.clone(),
                        assignee: i.assignee.clone(),
                    })
                    .collect();
                // Sort by priority ascending (critical first)
                self.tracked_issues
                    .sort_by(|a, b| a.priority.unwrap_or(99).cmp(&b.priority.unwrap_or(99)));
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("{} issues loaded", self.tracked_issues.len()),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::TurnComplete { session } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainError { session, message } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain error: {}", message),
                    kind: LogEntryKind::Error,
                });
            }
            SpurEventBody::BrainReconnecting {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnecting: {}", brain_name, reason),
                    kind: LogEntryKind::Info,
                });
            }
            SpurEventBody::BrainReconnected {
                session,
                brain_name,
                outcome,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                let (message, kind) = match outcome {
                    spur_acp::LoadOutcome::Restored => (
                        format!(
                            "Brain '{}' reconnected (state restored; your last prompt was dropped — retype)",
                            brain_name
                        ),
                        LogEntryKind::Info,
                    ),
                    spur_acp::LoadOutcome::FellBackToNew { reason } => (
                        format!(
                            "Brain '{}' reconnected — started FRESH ({}); retype to continue",
                            brain_name, reason
                        ),
                        LogEntryKind::Error,
                    ),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message,
                    kind,
                });
            }
            SpurEventBody::BrainReconnectFailed {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnect FAILED: {}", brain_name, reason),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::WorkerProgress {
                executor_id,
                name,
                pct,
                ..
            } => {
                let msg = match pct {
                    Some(p) => format!("{} ({}%)", name, p),
                    None => name.clone(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerFileTouched {
                executor_id,
                path,
                kind,
                ..
            } => {
                let verb = match kind {
                    spur_acp::FileTouchKind::Read => "read",
                    spur_acp::FileTouchKind::Write => "wrote",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: format!("{} {}", verb, path.display()),
                    kind: LogEntryKind::Act,
                });
            }

            SpurEventBody::GraphAlertsSummary {
                total,
                critical,
                warning,
                details,
            } => {
                self.alert_summary = Some((*total, *critical, *warning));
                for msg in details.iter().take(5) {
                    self.activity_log.push(LogEntry {
                        timestamp: Self::now_stamp(),
                        prefix: "[graph]".into(),
                        message: msg.clone(),
                        kind: if *critical > 0 {
                            LogEntryKind::Error
                        } else {
                            LogEntryKind::Info
                        },
                    });
                }
            }

            SpurEventBody::PlanTaskReviewed {
                plan_id: _,
                task_id,
                task_name,
                decision,
                feedback,
                attempt,
                max_attempts,
            } => {
                let (icon, label, kind) = match decision.as_str() {
                    "approve" => ("✓", "approved", LogEntryKind::Complete),
                    "reject" => ("✗", "rejected", LogEntryKind::Error),
                    "request_changes" => ("↻", "requested changes on", LogEntryKind::Act),
                    _ => ("?", "reviewed", LogEntryKind::Info),
                };
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!(" (attempt {attempt}/{max_attempts})")
                } else {
                    format!(" (attempt {attempt})")
                };
                let fb_suffix = feedback
                    .as_ref()
                    .map(|f| format!(": \"{}\"", truncate_display(f, 60)))
                    .unwrap_or_default();
                // Distinct entry when attempt budget is exhausted by a reject.
                let exhausted =
                    decision == "reject" && *max_attempts > 0 && *attempt >= *max_attempts;
                let message = if exhausted {
                    format!(
                        "✗ Task \"{display}\" failed — max attempts ({max_attempts}) reached{fb_suffix}"
                    )
                } else {
                    format!("{icon} Brain {label} \"{display}\"{attempts_suffix}{fb_suffix}")
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message,
                    kind,
                });
            }
            SpurEventBody::PlanTaskIterating {
                plan_id: _,
                task_id,
                task_name,
                attempt,
                max_attempts,
                delegation_id: _,
            } => {
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!("{attempt}/{max_attempts}")
                } else {
                    format!("{attempt}")
                };
                let final_hint = if *max_attempts > 0 && *attempt >= *max_attempts {
                    " — final"
                } else {
                    ""
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "↻ Re-dispatched \"{display}\" (attempt {attempts_suffix}{final_hint})"
                    ),
                    kind: LogEntryKind::Act,
                });
            }

            SpurEventBody::OrphanReaped {
                agent_name,
                pgid,
                age_secs,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[orphan:{}]", agent_name),
                    message: format!("Reaped orphan (pgid {pgid}, age {age_secs}s)"),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::PlanPendingSweep {
                plan_id,
                epic_id,
                action,
                child_count,
                age_secs,
                reason,
            } => {
                let plan = plan_id.as_deref().unwrap_or("unknown");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "Pending sweep {action} epic {epic_id} for {plan} ({child_count} children, age {age_secs}s): {reason}"
                    ),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::DispatchLeaseExpired {
                plan_id,
                task_id,
                issue_id,
                delegation_id,
                expired_at,
                age_secs,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "Dispatch lease expired for {plan_id}/{task_id} ({issue_id}, {delegation_id}, expired at {expired_at}, age {age_secs}s)"
                    ),
                    kind: LogEntryKind::Error,
                });
            }

            _ => {}
        }
    }

    fn tick(&mut self) {
        self.tick_and_report_flush();
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        // NOTE: App::render bypasses this method and calls render_with_lineage
        // directly so it can pass worker_streams. This fallback exists only to
        // satisfy the View trait (e.g., in tests that don't need stream traces).
        let mut empty_ws = crate::worker_streams::WorkerStreams::new();
        self.render_with_lineage(frame, area, &mut empty_ws, ctx);
    }
}

impl DashboardView {
    /// Handle a key event with access to per-executor `ReactTrace` instances.
    /// App calls this directly instead of `View::handle_key` so that scroll
    /// actions on the Stream tab are routed to the focused executor's trace.
    pub fn handle_key_with_worker_streams(
        &mut self,
        key: KeyEvent,
        lineage: &spur_core::lineage::projection::ExecutorLineage,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) -> Option<crate::action::Action> {
        self.handle_key_inner(key, Some(lineage), worker_streams)
    }

    /// Tick + flush batched text. Returns true iff at least one batch was
    /// flushed to the activity log (so the caller can mark the TUI dirty).
    pub fn tick_and_report_flush(&mut self) -> bool {
        self.input_bar.tick();
        self.agents_tree.tick();

        // Flush text batches older than 500ms
        let threshold = std::time::Duration::from_millis(500);
        let now = Instant::now();
        let expired: Vec<String> = self
            .text_batch
            .iter()
            .filter(|(_, (_, ts))| now.duration_since(*ts) > threshold)
            .map(|(k, _)| k.clone())
            .collect();
        let flushed_any = !expired.is_empty();

        for session_id in expired {
            if let Some((text, _)) = self.text_batch.remove(&session_id) {
                let prefix = Self::prefix_for_session(&session_id);
                // Take the last 50 chars for a condensed view
                let display = if text.len() > 50 {
                    let mut start = text.len() - 50;
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    format!("\u{25b8} ...{}", &text[start..])
                } else {
                    format!("\u{25b8} {}", text)
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: display,
                    kind: LogEntryKind::Think,
                });
            }
        }

        flushed_any
    }

    /// Test-only: read current InputBar text.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_text_for_test(&self) -> String {
        self.input_bar.text()
    }

    /// Test-only: read the currently displayed example prompt.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn current_example_prompt_for_test(&self) -> &str {
        self.example_prompts
            .get(self.example_index)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Test-only: whether Dashboard's completion picker is open.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn completion_active_for_test(&self) -> bool {
        self.completion.is_active()
    }

    /// Test-only: mutable InputBar access for seeding text in tests.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_mut_for_test(&mut self) -> &mut crate::components::input_bar::InputBar {
        &mut self.input_bar
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn command_registry_for_test(&self) -> &crate::commands::CommandRegistry {
        &self.command_registry
    }
}

#[cfg(all(test, feature = "analytics"))]
mod live_cost_cache_tests {
    use super::*;
    use crate::app::LiveCostCache;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn sid(value: &str) -> spur_acp::SessionId {
        spur_acp::SessionId(value.to_string())
    }

    fn cache_with(session_id: spur_acp::SessionId, cost: f64) -> Arc<RwLock<LiveCostCache>> {
        Arc::new(RwLock::new(LiveCostCache {
            by_session: HashMap::from([(session_id, cost)]),
            last_refresh: chrono::Utc::now(),
            last_error: None,
        }))
    }

    fn empty_cache() -> Arc<RwLock<LiveCostCache>> {
        Arc::new(RwLock::new(LiveCostCache::default()))
    }

    fn lineage_with_cost(session_id: spur_acp::SessionId, cost: f64) -> ExecutorLineage {
        let mut lineage = ExecutorLineage::new();
        lineage.apply(&spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::WorkerSpawned {
                agent: "codex".to_string(),
                session: session_id.clone(),
                worktree: std::path::PathBuf::new(),
            },
        ));
        lineage.apply(&spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::CostUpdate {
                session: session_id,
                agent: "codex".to_string(),
                estimated_cost_usd: cost,
            },
        ));
        lineage
    }

    fn lineage_with_shared_session_costs(
        session_id: spur_acp::SessionId,
        costs: impl IntoIterator<Item = f64>,
    ) -> ExecutorLineage {
        let mut lineage = ExecutorLineage::new();
        for (idx, cost) in costs.into_iter().enumerate() {
            let node_id = format!("node-{}", idx + 1);
            lineage.apply(&spur_acp::SpurEvent::now(
                spur_acp::SpurEventBody::ExecutorSpawned {
                    id: node_id.clone(),
                    parent_id: None,
                    session_id: session_id.clone(),
                    agent: "codex".to_string(),
                    role: spur_acp::Role::Executor,
                    task_spec: format!("task {}", idx + 1),
                },
            ));
            lineage.apply(&spur_acp::SpurEvent::now(
                spur_acp::SpurEventBody::CostUpdate {
                    session: sid(&node_id),
                    agent: "codex".to_string(),
                    estimated_cost_usd: cost,
                },
            ));
        }
        lineage
    }

    fn render_dashboard_text(dash: &mut DashboardView, lineage: &ExecutorLineage) -> String {
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut worker_streams = crate::worker_streams::WorkerStreams::new();
        let ctx = crate::views::ViewContext::test_ctx(lineage);
        terminal
            .draw(|frame| {
                dash.render_with_lineage(
                    frame,
                    Rect::new(0, 0, 120, 20),
                    &mut worker_streams,
                    &ctx,
                );
            })
            .unwrap();

        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn dashboard_reads_from_live_cost_cache_when_present() {
        let session_id = sid("abc");
        let cache = cache_with(session_id.clone(), 4.21);
        let dash = DashboardView::with_cache(cache);

        assert_eq!(
            dash.current_cost(&session_id, &ExecutorLineage::new()),
            Some(4.21)
        );
    }

    #[test]
    fn dashboard_falls_through_to_lineage_when_cache_cold() {
        let session_id = sid("xyz");
        let cache = Arc::new(RwLock::new(LiveCostCache::default()));
        let dash = DashboardView::with_cache(cache);
        let lineage = lineage_with_cost(session_id.clone(), 2.10);

        assert_eq!(dash.current_cost(&session_id, &lineage), Some(2.10));
    }

    #[test]
    fn dashboard_falls_through_when_cache_session_missing() {
        let session_id = sid("xyz");
        let cache = cache_with(sid("other"), 4.21);
        let dash = DashboardView::with_cache(cache);
        let lineage = lineage_with_cost(session_id.clone(), 2.10);

        assert_eq!(dash.current_cost(&session_id, &lineage), Some(2.10));
    }

    #[test]
    fn dashboard_total_cost_dedupes_cached_session_across_multiple_nodes() {
        let session_id = sid("S");
        let lineage = lineage_with_shared_session_costs(session_id.clone(), [10.0, 20.0, 30.0]);
        let cache = cache_with(session_id, 100.0);
        let mut dash = DashboardView::with_cache(cache);

        let text = render_dashboard_text(&mut dash, &lineage);

        assert!(
            text.contains("$100.00"),
            "expected cached session cost to be counted once, got:\n{text}"
        );
    }

    #[test]
    fn dashboard_total_cost_sums_per_node_when_cache_cold() {
        let session_id = sid("S");
        let lineage = lineage_with_shared_session_costs(session_id.clone(), [10.0, 20.0, 30.0]);
        let cache = empty_cache();
        let mut dash = DashboardView::with_cache(cache);

        let text = render_dashboard_text(&mut dash, &lineage);

        assert!(
            text.contains("$60.00"),
            "expected cold cache to use the per-node sum, got:\n{text}"
        );

        let non_ambiguous_lineage =
            lineage_with_shared_session_costs(session_id, [10.0, 20.0, 40.0]);
        let cache = empty_cache();
        let mut dash = DashboardView::with_cache(cache);

        let text = render_dashboard_text(&mut dash, &non_ambiguous_lineage);

        assert!(
            text.contains("$70.00"),
            "expected cold cache to avoid first-match aggregation, got:\n{text}"
        );
    }
}

#[cfg(all(test, not(feature = "analytics")))]
mod total_cost_no_analytics_tests {
    use super::*;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    fn sid(value: &str) -> spur_acp::SessionId {
        spur_acp::SessionId(value.to_string())
    }

    fn lineage_with_shared_session_costs(
        session_id: spur_acp::SessionId,
        costs: impl IntoIterator<Item = f64>,
    ) -> ExecutorLineage {
        let mut lineage = ExecutorLineage::new();
        for (idx, cost) in costs.into_iter().enumerate() {
            let node_id = format!("node-{}", idx + 1);
            lineage.apply(&spur_acp::SpurEvent::now(
                spur_acp::SpurEventBody::ExecutorSpawned {
                    id: node_id.clone(),
                    parent_id: None,
                    session_id: session_id.clone(),
                    agent: "codex".to_string(),
                    role: spur_acp::Role::Executor,
                    task_spec: format!("task {}", idx + 1),
                },
            ));
            lineage.apply(&spur_acp::SpurEvent::now(
                spur_acp::SpurEventBody::CostUpdate {
                    session: sid(&node_id),
                    agent: "codex".to_string(),
                    estimated_cost_usd: cost,
                },
            ));
        }
        lineage
    }

    fn render_dashboard_text(dash: &mut DashboardView, lineage: &ExecutorLineage) -> String {
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut worker_streams = crate::worker_streams::WorkerStreams::new();
        let ctx = crate::views::ViewContext::test_ctx(lineage);
        terminal
            .draw(|frame| {
                dash.render_with_lineage(
                    frame,
                    Rect::new(0, 0, 120, 20),
                    &mut worker_streams,
                    &ctx,
                );
            })
            .unwrap();

        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn dashboard_total_cost_no_analytics_uses_per_node_sum() {
        let lineage = lineage_with_shared_session_costs(sid("S"), [10.0, 20.0, 40.0]);
        let mut dash = DashboardView::new();

        let text = render_dashboard_text(&mut dash, &lineage);

        assert!(
            text.contains("$70.00"),
            "expected no-analytics total to use the per-node sum, got:\n{text}"
        );
    }
}
