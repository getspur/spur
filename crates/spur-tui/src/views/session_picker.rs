use std::cell::Cell;

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
use crate::components::status_bar::{StatusBar, StatusBarProps};
use crate::session_metadata::SessionMetadata;

use super::View;

const FOOTER_HINT: &str = "j/k nav \u{00b7} Enter resume \u{00b7} / search \u{00b7} n new \u{00b7} R rename \u{00b7} d archive \u{00b7} a show-archived \u{00b7} p pin \u{00b7} P preview \u{00b7} r refresh \u{00b7} Esc back";

fn render_footer_hint(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Span::styled(
            FOOTER_HINT,
            Style::default().fg(Color::DarkGray),
        )),
        area,
    );
}

// ─── State ────────────────────────────────────────────────────────────

enum PickerState {
    Loading,
    Populated {
        agent: String,
        sessions: Vec<SessionInfo>,
        cursor: usize,
        resuming: bool,
        search_focused: bool,
        filter: String,
    },
    Error {
        message: String,
    },
}

// ─── View ─────────────────────────────────────────────────────────────

/// Active inline-rename session state (shown as a prompt in the bottom chunk).
struct RenameState {
    session_id: String,
    buffer: String,
}

/// Pending switch-confirm target. `Some` on the view means the banner is up
/// and the next `y`/`Enter` commits the encoded action.
enum ConfirmSwitchTarget {
    Resume(String),
    NewSession,
}

pub struct SessionPickerView {
    state: PickerState,
    /// Interior-mutable so render(&self) can adjust scroll position.
    scroll_offset: Cell<usize>,
    metadata: SessionMetadata,
    /// View-level toggle; when false, archived sessions are hidden.
    show_archived: bool,
    /// `Some` when the user pressed `R`; intercepts all keys until
    /// `Enter` commits or `Esc` cancels.
    rename_state: Option<RenameState>,
    /// Toggled via uppercase `P`; when true, renders a metadata preview
    /// pane below the list.
    preview_visible: bool,
    /// ID of the current session if it has an unsent draft; None otherwise.
    /// Used to decide whether Enter on a different session (or [+ New])
    /// should show the switch-safety confirm banner.
    current_session_with_draft: Option<String>,
    /// SPUR session id of the currently-active SessionDetail, if any.
    /// Distinct from `current_session_with_draft` (which is Some only when
    /// the active session has UNSENT draft text). Used so that Enter on the
    /// current session's row short-circuits to NavigateTo instead of
    /// pointlessly re-resuming the session the user is already in.
    current_session_id: Option<String>,
    /// `Some` when the confirm-switch banner is up. Encodes what to do on
    /// `y`/`Enter` confirm: resume the given id, or start a new session.
    confirm_switch: Option<ConfirmSwitchTarget>,
}

impl Default for SessionPickerView {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPickerView {
    pub fn new() -> Self {
        Self {
            state: PickerState::Loading,
            scroll_offset: Cell::new(0),
            metadata: SessionMetadata::default(),
            show_archived: false,
            rename_state: None,
            preview_visible: false,
            current_session_with_draft: None,
            current_session_id: None,
            confirm_switch: None,
        }
    }

    pub fn is_rename_active(&self) -> bool {
        self.rename_state.is_some()
    }

    pub fn is_preview_visible(&self) -> bool {
        self.preview_visible
    }

    /// Called by App whenever metadata changes OR picker is opened. Passes in
    /// the id of the current session if it has an unsent draft, else None.
    pub fn set_current_session_has_draft(&mut self, session_id: Option<String>) {
        self.current_session_with_draft = session_id;
    }

    /// Push the SPUR session id of the currently-active SessionDetail into
    /// the picker. `None` when Dashboard is the active view.
    pub fn set_current_session_id(&mut self, session_id: Option<String>) {
        self.current_session_id = session_id;
    }

    pub fn is_confirm_switch_visible(&self) -> bool {
        self.confirm_switch.is_some()
    }

    pub fn set_metadata(&mut self, metadata: SessionMetadata) {
        self.metadata = metadata;
    }

    pub fn toggle_show_archived(&mut self) {
        self.show_archived = !self.show_archived;
    }

    pub fn is_show_archived(&self) -> bool {
        self.show_archived
    }

    pub fn cursor(&self) -> usize {
        match &self.state {
            PickerState::Populated { cursor, .. } => *cursor,
            _ => 0,
        }
    }

    pub fn filter(&self) -> String {
        match &self.state {
            PickerState::Populated { filter, .. } => filter.clone(),
            _ => String::new(),
        }
    }

    pub fn set_sessions(&mut self, agent: String, sessions: Vec<SessionInfo>) {
        let (prev_cursor, prev_filter) = match &self.state {
            PickerState::Populated { cursor, filter, .. } => (*cursor, filter.clone()),
            _ => (0, String::new()),
        };
        // Clamp cursor to new session list length (cursor max is sessions.len();
        // 0 = [+ New] row, 1..=len = sessions).
        let max_cursor = sessions.len();
        let clamped_cursor = prev_cursor.min(max_cursor);

        self.state = PickerState::Populated {
            agent,
            sessions,
            cursor: clamped_cursor,
            resuming: false,
            search_focused: false,
            filter: prev_filter,
        };
        self.scroll_offset.set(0);
    }

    fn highlighted_session_id(&self) -> Option<String> {
        let PickerState::Populated {
            sessions,
            cursor,
            filter,
            ..
        } = &self.state
        else {
            return None;
        };
        if *cursor == 0 {
            return None;
        }
        let indices = Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived);
        let real_idx = indices.get(*cursor - 1).copied()?;
        Some(sessions[real_idx].session_id.0.as_ref().to_string())
    }

    fn filtered_indices(
        sessions: &[SessionInfo],
        filter: &str,
        metadata: &SessionMetadata,
        show_archived: bool,
    ) -> Vec<usize> {
        let candidates: Vec<usize> = (0..sessions.len())
            .filter(|&i| {
                let archived = metadata
                    .sessions
                    .get(sessions[i].session_id.0.as_ref())
                    .map(|e| e.archived)
                    .unwrap_or(false);
                if archived {
                    show_archived
                } else {
                    true
                }
            })
            .collect();

        if filter.is_empty() {
            let mut all = candidates;
            all.sort_by(|&a, &b| {
                let ea = metadata.sessions.get(sessions[a].session_id.0.as_ref());
                let eb = metadata.sessions.get(sessions[b].session_id.0.as_ref());
                let pa = ea.map(|e| e.pinned).unwrap_or(false);
                let pb = eb.map(|e| e.pinned).unwrap_or(false);
                match (pa, pb) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => {
                        // recency desc via updated_at (newest first)
                        let ta = sessions[a].updated_at.as_deref().unwrap_or("");
                        let tb = sessions[b].updated_at.as_deref().unwrap_or("");
                        tb.cmp(ta)
                    }
                }
            });
            return all;
        }
        use nucleo_matcher::{
            pattern::{CaseMatching, Normalization, Pattern},
            Matcher,
        };
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = candidates
            .into_iter()
            .filter_map(|i| {
                let session = &sessions[i];
                let title = Self::resolved_title(session, metadata, false);
                let cwd = session.cwd.display().to_string();
                let id = session.session_id.0.as_ref();
                let haystack = format!("{title} {cwd} {id}");
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    pub fn visible_session_count(&self) -> usize {
        match &self.state {
            PickerState::Populated {
                sessions, filter, ..
            } => Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived).len(),
            _ => 0,
        }
    }

    pub fn visible_session_at(&self, idx: usize) -> Option<&SessionInfo> {
        match &self.state {
            PickerState::Populated {
                sessions, filter, ..
            } => Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived)
                .get(idx)
                .and_then(|&i| sessions.get(i)),
            _ => None,
        }
    }

    pub fn set_error(&mut self, message: String) {
        self.state = PickerState::Error { message };
    }

    pub fn handle_paste(&mut self, text: &str) {
        let first_line = text.lines().next().unwrap_or("");
        if first_line.is_empty() {
            return;
        }
        if let Some(ref mut rs) = self.rename_state {
            rs.buffer.push_str(first_line);
            return;
        }
        if let PickerState::Populated {
            search_focused: true,
            filter,
            cursor,
            ..
        } = &mut self.state
        {
            filter.push_str(first_line);
            *cursor = 0;
        }
    }

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

    fn cwds_are_heterogeneous(sessions: &[SessionInfo]) -> bool {
        if sessions.len() <= 1 {
            return false;
        }
        let first = &sessions[0].cwd;
        sessions.iter().any(|s| s.cwd != *first)
    }

    fn cwd_basename(cwd: &std::path::Path) -> &str {
        cwd.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| cwd.to_str().unwrap_or(""))
    }

    fn display_text(session: &SessionInfo, show_cwd: bool) -> String {
        if let Some(ref title) = session.title {
            title.clone()
        } else if show_cwd {
            format!("{}/", Self::cwd_basename(&session.cwd))
        } else {
            "(untitled session)".to_string()
        }
    }

    fn resolved_title(
        session: &spur_acp::SessionInfo,
        metadata: &SessionMetadata,
        show_cwd: bool,
    ) -> String {
        if let Some(entry) = metadata.sessions.get(session.session_id.0.as_ref()) {
            if let Some(ref t) = entry.title_override {
                if !t.is_empty() {
                    return t.clone();
                }
            }
        }
        Self::display_text(session, show_cwd)
    }

    fn render_loading(
        &self,
        frame: &mut Frame,
        area: Rect,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
    ) {
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
                Span::styled(
                    " \u{00b7}\u{00b7}\u{00b7}",
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ];
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
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
        StatusBar::render(
            frame,
            chunks[1],
            StatusBarProps {
                view: &ViewId::SessionPicker,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                issue_count: 0,
                alert_summary: None,
                license_badge,
            },
        );
        render_footer_hint(frame, chunks[2]);
    }

    // Refactoring to a props struct is deferred — the signature is stable and
    // every caller already passes every arg. See `StatusBarProps` for the
    // pattern if/when we do fold these.
    #[allow(clippy::too_many_arguments)]
    fn render_populated(
        &self,
        frame: &mut Frame,
        area: Rect,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        agent: &str,
        sessions: &[SessionInfo],
        cursor: usize,
        resuming: bool,
        search_focused: bool,
        filter: &str,
    ) {
        let show_cwd = Self::cwds_are_heterogeneous(sessions);
        let visible_height = area.height.saturating_sub(4) as usize;

        let indices = Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived);

        // Clamp scroll_offset so cursor is always visible.
        // `cursor` indexes a virtual list where 0 = [+ New session] row and
        // 1..=filtered.len() are real (visible) sessions. We scroll the real
        // sessions; the [+ New session] row is always visible as the first entry.
        let mut scroll = self.scroll_offset.get();
        let session_cursor = cursor.saturating_sub(1);
        if cursor >= 1 && session_cursor >= scroll + visible_height {
            scroll = session_cursor.saturating_sub(visible_height.saturating_sub(1));
        }
        if cursor >= 1 && session_cursor < scroll {
            scroll = session_cursor;
        }
        self.scroll_offset.set(scroll);

        let mut header_spans = vec![
            Span::styled(
                "Sessions ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("({})", agent), Style::default().fg(Color::DarkGray)),
        ];
        if self.show_archived {
            header_spans.push(Span::styled(
                " [showing archived]",
                Style::default().fg(Color::DarkGray),
            ));
        }
        let mut lines = vec![
            Line::from(header_spans),
            Line::from(vec![
                Span::styled("  Search  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}{}", filter, if search_focused { "_" } else { "" }),
                    Style::default().fg(if search_focused {
                        Color::Cyan
                    } else {
                        Color::Gray
                    }),
                ),
            ]),
            Line::from(""),
        ];

        // [+ Start new session] virtual row.
        let is_new_selected = cursor == 0;
        let new_prefix = if is_new_selected { "\u{25b8} " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                new_prefix,
                if is_new_selected {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                "+ Start new session",
                if is_new_selected {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "  \u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(Color::DarkGray),
        )));

        for (display_i, real_i) in indices.iter().enumerate().skip(scroll).take(visible_height) {
            let session = &sessions[*real_i];
            let is_selected = cursor == display_i + 1;
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let raw_id = session.session_id.0.as_ref();
            let short_id = &raw_id[..8.min(raw_id.len())];
            let display = Self::resolved_title(session, &self.metadata, show_cwd);
            let time_str = session
                .updated_at
                .as_deref()
                .map(Self::relative_time)
                .unwrap_or_default();

            let suffix = if is_selected && resuming {
                " loading...".to_string()
            } else if show_cwd {
                format!("  {}/", Self::cwd_basename(&session.cwd))
            } else {
                String::new()
            };

            let archived = self
                .metadata
                .sessions
                .get(session.session_id.0.as_ref())
                .map(|e| e.archived)
                .unwrap_or(false);

            let style = if archived {
                Style::default().fg(Color::DarkGray)
            } else if is_selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            let id_style = if archived {
                Style::default().fg(Color::DarkGray)
            } else if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };

            let pinned = self
                .metadata
                .sessions
                .get(session.session_id.0.as_ref())
                .map(|e| e.pinned)
                .unwrap_or(false);
            let mut spans: Vec<Span> = Vec::with_capacity(8);
            spans.push(Span::styled(prefix, style));
            if pinned {
                spans.push(Span::styled(
                    "\u{2b50} ",
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::styled(short_id, id_style));
            spans.push(Span::styled(
                " \u{00b7} ",
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(display, style));
            spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(time_str, Style::default().fg(Color::DarkGray)));
            if archived {
                spans.push(Span::styled(
                    " [archived]",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(spans));
        }

        let preview_height: u16 = 8;
        let chunks = if self.preview_visible {
            Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(preview_height),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area)
        } else {
            Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area)
        };
        frame.render_widget(Paragraph::new(lines), chunks[0]);

        // When preview is visible, the status/footer chunks shift by one.
        let (status_idx, footer_idx) = if self.preview_visible { (2, 3) } else { (1, 2) };

        if self.preview_visible {
            use crate::components::session_preview::{PreviewContent, SessionPreview};
            let content = if cursor == 0 {
                PreviewContent {
                    placeholder: Some(
                        "Press Enter to start a new session \u{00b7} any unsent draft will be saved"
                            .to_string(),
                    ),
                    ..Default::default()
                }
            } else {
                let indices =
                    Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived);
                let real_idx = indices.get(cursor - 1).copied();
                if let Some(i) = real_idx {
                    let session = &sessions[i];
                    let id = session.session_id.0.as_ref().to_string();
                    let cwd = session.cwd.display().to_string();
                    let updated = session.updated_at.clone().unwrap_or_default();
                    let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
                    let pinned = entry.map(|e| e.pinned).unwrap_or(false);
                    let archived = entry.map(|e| e.archived).unwrap_or(false);
                    let draft = entry.map(|e| e.draft.clone()).unwrap_or_default();

                    let mut rows = vec![("Session".into(), id), ("CWD".into(), cwd)];
                    if !updated.is_empty() {
                        rows.push(("Updated".into(), updated));
                    }
                    if pinned {
                        rows.push(("Pinned".into(), "\u{2b50}".into()));
                    }
                    if archived {
                        rows.push(("Archived".into(), "yes".into()));
                    }
                    if !draft.is_empty() {
                        let truncated = if draft.chars().count() > 80 {
                            let t: String = draft.chars().take(80).collect();
                            format!("{t}\u{2026}")
                        } else {
                            draft.clone()
                        };
                        rows.push(("Draft".into(), truncated));
                    }
                    PreviewContent {
                        rows,
                        placeholder: None,
                    }
                } else {
                    PreviewContent::default()
                }
            };
            SessionPreview::render(frame, chunks[1], &content);
        }

        if let Some(ref target) = self.confirm_switch {
            let current = self
                .current_session_with_draft
                .as_deref()
                .unwrap_or("current session");
            let action_desc = match target {
                ConfirmSwitchTarget::Resume(id) => format!("resume {}", id),
                ConfirmSwitchTarget::NewSession => "start a new session".to_string(),
            };
            let prompt = format!(
                "Session \"{current}\" has an unsent draft — save and {action_desc}? [y/N]"
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    prompt,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                chunks[status_idx],
            );
        } else if let Some(ref rs) = self.rename_state {
            let prompt = format!("Rename \u{2192} {}_", rs.buffer);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    prompt,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                chunks[status_idx],
            );
        } else {
            StatusBar::render(
                frame,
                chunks[status_idx],
                StatusBarProps {
                    view: &ViewId::SessionPicker,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0m 00s",
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    issue_count: 0,
                    alert_summary: None,
                    license_badge,
                },
            );
        }
        render_footer_hint(frame, chunks[footer_idx]);
    }

    fn render_error(
        &self,
        frame: &mut Frame,
        area: Rect,
        message: &str,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
    ) {
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
        StatusBar::render(
            frame,
            chunks[1],
            StatusBarProps {
                view: &ViewId::SessionPicker,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0m 00s",
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                issue_count: 0,
                alert_summary: None,
                license_badge,
            },
        );
        render_footer_hint(frame, chunks[2]);
    }
}

impl View for SessionPickerView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        // 0. Confirm-switch intercepts all keys until y/Enter commits or anything else cancels.
        if let Some(ref target) = self.confirm_switch {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let out = match target {
                        ConfirmSwitchTarget::Resume(id) => Action::ResumeSession {
                            session_id: id.clone(),
                        },
                        ConfirmSwitchTarget::NewSession => Action::NewSessionRequested,
                    };
                    self.confirm_switch = None;
                    return Some(out);
                }
                _ => {
                    // n / N / Esc / anything else cancels.
                    self.confirm_switch = None;
                    return None;
                }
            }
        }

        // 1. Rename-mode intercepts all keys until Enter/Esc.
        if let Some(rs) = self.rename_state.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let out = Action::RenameSession {
                        session_id: rs.session_id.clone(),
                        new_title: rs.buffer.clone(),
                    };
                    self.rename_state = None;
                    return Some(out);
                }
                KeyCode::Esc => {
                    self.rename_state = None;
                    return None;
                }
                KeyCode::Backspace => {
                    rs.buffer.pop();
                    return None;
                }
                KeyCode::Char(c) => {
                    rs.buffer.push(c);
                    return None;
                }
                _ => return None,
            }
        }

        // Preview toggle intercepts before list-mode logic. Only valid when
        // search isn't focused (so capital P typed in search box still filters).
        let can_toggle_preview = matches!(
            &self.state,
            PickerState::Populated {
                search_focused: false,
                ..
            }
        );
        if can_toggle_preview {
            if let KeyCode::Char('P') = key.code {
                self.preview_visible = !self.preview_visible;
                return None;
            }
        }

        // Compute once — needed by list-mode p/R/d arms before we split-borrow.
        let hl_session_id = self.highlighted_session_id();

        // Deferred state transitions to apply after the split borrow ends.
        enum Post {
            None,
            StartRename { session_id: String, buffer: String },
            StartConfirmSwitch(ConfirmSwitchTarget),
        }
        let mut post = Post::None;

        // Split-borrow self so we can reach `metadata` while also mutably
        // borrowing `state`.
        let action = {
            let SessionPickerView {
                state,
                metadata,
                show_archived,
                current_session_with_draft,
                current_session_id,
                ..
            } = self;
            match state {
                PickerState::Populated {
                    sessions,
                    cursor,
                    resuming,
                    search_focused,
                    filter,
                    ..
                } => {
                    if *resuming {
                        return None;
                    }

                    if *search_focused {
                        match key.code {
                            KeyCode::Esc => {
                                *search_focused = false;
                                None
                            }
                            KeyCode::Enter => {
                                *search_focused = false;
                                None
                            }
                            KeyCode::Backspace => {
                                filter.pop();
                                *cursor = 0;
                                None
                            }
                            KeyCode::Char(c) => {
                                filter.push(c);
                                *cursor = 0;
                                None
                            }
                            _ => None,
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('/') => {
                                *search_focused = true;
                                None
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if *cursor > 0 {
                                    *cursor -= 1;
                                }
                                None
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let visible = Self::filtered_indices(
                                    sessions,
                                    filter,
                                    metadata,
                                    *show_archived,
                                )
                                .len();
                                if *cursor < visible {
                                    *cursor += 1;
                                }
                                None
                            }
                            KeyCode::Char('n') => Some(Action::NewSessionRequested),
                            KeyCode::Enter => {
                                if *cursor == 0 {
                                    // [+ New session] row: if the current session has a
                                    // draft, ask the user to confirm switching away.
                                    if current_session_with_draft.is_some() {
                                        post = Post::StartConfirmSwitch(
                                            ConfirmSwitchTarget::NewSession,
                                        );
                                        None
                                    } else {
                                        Some(Action::NewSessionRequested)
                                    }
                                } else {
                                    let indices = Self::filtered_indices(
                                        sessions,
                                        filter,
                                        metadata,
                                        *show_archived,
                                    );
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
                                            post = Post::StartConfirmSwitch(
                                                ConfirmSwitchTarget::Resume(sid),
                                            );
                                            None
                                        } else {
                                            *resuming = true;
                                            Some(Action::ResumeSession { session_id: sid })
                                        }
                                    }
                                }
                            }
                            KeyCode::Esc => {
                                if !filter.is_empty() {
                                    filter.clear();
                                    *cursor = 0;
                                    None
                                } else {
                                    Some(Action::NavigateTo(ViewId::Dashboard))
                                }
                            }
                            KeyCode::Char('p') => hl_session_id
                                .clone()
                                .map(|session_id| Action::ToggleSessionPin { session_id }),
                            KeyCode::Char('d') => hl_session_id
                                .clone()
                                .map(|session_id| Action::ToggleSessionArchive { session_id }),
                            KeyCode::Char('a') => Some(Action::ToggleShowArchived),
                            KeyCode::Char('r') => Some(Action::RefreshSessions),
                            KeyCode::Char('R') => {
                                if let Some(ref sid) = hl_session_id {
                                    let buffer = sessions
                                        .iter()
                                        .find(|s| s.session_id.0.as_ref() == sid.as_str())
                                        .map(|s| Self::resolved_title(s, metadata, false))
                                        .unwrap_or_default();
                                    post = Post::StartRename {
                                        session_id: sid.clone(),
                                        buffer,
                                    };
                                }
                                None
                            }
                            _ => None,
                        }
                    }
                }
                PickerState::Loading | PickerState::Error { .. } => match key.code {
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
                },
            }
        };

        // Apply deferred state transitions.
        match post {
            Post::None => {}
            Post::StartRename { session_id, buffer } => {
                self.rename_state = Some(RenameState { session_id, buffer });
            }
            Post::StartConfirmSwitch(target) => {
                self.confirm_switch = Some(target);
            }
        }

        action
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {
        // SessionsListed and SessionsListError are handled by App,
        // which calls set_sessions() or set_error() directly.
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        match &self.state {
            PickerState::Loading => self.render_loading(frame, area, ctx.license_badge),
            PickerState::Populated {
                agent,
                sessions,
                cursor,
                resuming,
                search_focused,
                filter,
            } => self.render_populated(
                frame,
                area,
                ctx.license_badge,
                agent,
                sessions,
                *cursor,
                *resuming,
                *search_focused,
                filter,
            ),
            PickerState::Error { message } => {
                self.render_error(frame, area, message, ctx.license_badge)
            }
        }
    }

    fn tick(&mut self) {
        // No animations in the picker.
    }
}

#[cfg(test)]
mod current_session_shortcut_tests {
    use super::*;
    use crate::action::{Action, ViewId};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        crate::views::ViewContext {
            lineage: &LINEAGE,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
        }
    }

    fn make_session(id: &str) -> SessionInfo {
        SessionInfo::new(id.to_string(), PathBuf::from("/tmp"))
    }

    #[test]
    fn enter_on_current_session_row_navigates_back() {
        let mut picker = SessionPickerView::new();
        picker.set_sessions("test-brain".into(), vec![make_session("A")]);
        picker.set_current_session_id(Some("A".into()));

        // Cursor starts at 0 ([+ New session]); move to 1 (the A row).
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );

        let action = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &test_ctx(),
        );
        match action {
            Some(Action::NavigateTo(ViewId::SessionDetail(sid))) => {
                assert_eq!(sid.0, "A");
            }
            other => panic!("expected NavigateTo(SessionDetail(A)), got {:?}", other),
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

        // Move cursor to row index 2 = session B.
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );
        picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &test_ctx(),
        );

        let action = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &test_ctx(),
        );
        match action {
            Some(Action::ResumeSession { session_id }) => {
                assert_eq!(session_id, "B");
            }
            other => panic!("expected ResumeSession(B), got {:?}", other),
        }
    }
}
