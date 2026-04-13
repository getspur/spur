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

pub struct SessionPickerView {
    state: PickerState,
    /// Interior-mutable so render(&self) can adjust scroll position.
    scroll_offset: Cell<usize>,
    metadata: SessionMetadata,
}

impl SessionPickerView {
    pub fn new() -> Self {
        Self {
            state: PickerState::Loading,
            scroll_offset: Cell::new(0),
            metadata: SessionMetadata::default(),
        }
    }

    pub fn set_metadata(&mut self, metadata: SessionMetadata) {
        self.metadata = metadata;
    }

    pub fn set_sessions(&mut self, agent: String, sessions: Vec<SessionInfo>) {
        self.state = PickerState::Populated {
            agent,
            sessions,
            cursor: 0,
            resuming: false,
            search_focused: false,
            filter: String::new(),
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
        let indices = Self::filtered_indices(sessions, filter, &self.metadata);
        let real_idx = indices.get(*cursor - 1).copied()?;
        Some(sessions[real_idx].session_id.0.as_ref().to_string())
    }

    fn filtered_indices(
        sessions: &[SessionInfo],
        filter: &str,
        metadata: &SessionMetadata,
    ) -> Vec<usize> {
        if filter.is_empty() {
            let mut all: Vec<usize> = (0..sessions.len()).collect();
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
        let mut scored: Vec<(u32, usize)> = sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let title = Self::resolved_title(s, metadata, false);
                let cwd = s.cwd.display().to_string();
                let id = s.session_id.0.as_ref();
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
            } => Self::filtered_indices(sessions, filter, &self.metadata).len(),
            _ => 0,
        }
    }

    pub fn visible_session_at(&self, idx: usize) -> Option<&SessionInfo> {
        match &self.state {
            PickerState::Populated {
                sessions, filter, ..
            } => Self::filtered_indices(sessions, filter, &self.metadata)
                .get(idx)
                .and_then(|&i| sessions.get(i)),
            _ => None,
        }
    }

    pub fn set_error(&mut self, message: String) {
        self.state = PickerState::Error { message };
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

    fn render_loading(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
            },
        );
        render_footer_hint(frame, chunks[2]);
    }

    fn render_populated(
        &self,
        frame: &mut Frame,
        area: Rect,
        agent: &str,
        sessions: &[SessionInfo],
        cursor: usize,
        resuming: bool,
        search_focused: bool,
        filter: &str,
    ) {
        let show_cwd = Self::cwds_are_heterogeneous(sessions);
        let visible_height = area.height.saturating_sub(4) as usize;

        let indices = Self::filtered_indices(sessions, filter, &self.metadata);

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

        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "Sessions ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({})", agent),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
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

        for (display_i, real_i) in indices.iter().enumerate().skip(scroll).take(visible_height)
        {
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

            let style = if is_selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            let id_style = if is_selected {
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
            spans.push(Span::styled(
                time_str,
                Style::default().fg(Color::DarkGray),
            ));
            lines.push(Line::from(spans));
        }

        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        frame.render_widget(Paragraph::new(lines), chunks[0]);
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
            },
        );
        render_footer_hint(frame, chunks[2]);
    }

    fn render_error(&self, frame: &mut Frame, area: Rect, message: &str) {
        let lines = vec![
            Line::from(Span::styled(
                "Sessions",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
            },
        );
        render_footer_hint(frame, chunks[2]);
    }
}

impl View for SessionPickerView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Compute once — needed by list-mode p/R/d arms before we split-borrow.
        let hl_session_id = self.highlighted_session_id();
        // Split-borrow self so we can reach `metadata` while also mutably
        // borrowing `state`.
        let SessionPickerView {
            state, metadata, ..
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
                            let visible =
                                Self::filtered_indices(sessions, filter, metadata).len();
                            if *cursor < visible {
                                *cursor += 1;
                            }
                            None
                        }
                        KeyCode::Char('n') => Some(Action::NewSessionRequested),
                        KeyCode::Enter => {
                            if *cursor == 0 {
                                Some(Action::NewSessionRequested)
                            } else {
                                let indices =
                                    Self::filtered_indices(sessions, filter, metadata);
                                let real_idx = indices.get(*cursor - 1).copied()?;
                                let sid = sessions[real_idx].session_id.0.to_string();
                                *resuming = true;
                                Some(Action::ResumeSession { session_id: sid })
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
                            .map(|session_id| Action::ToggleSessionPin { session_id }),
                        _ => None,
                    }
                }
            }
            PickerState::Loading | PickerState::Error { .. } => match key.code {
                KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                _ => None,
            },
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
                search_focused,
                filter,
            } => self.render_populated(
                frame,
                area,
                agent,
                sessions,
                *cursor,
                *resuming,
                *search_focused,
                filter,
            ),
            PickerState::Error { message } => self.render_error(frame, area, message),
        }
    }

    fn tick(&mut self) {
        // No animations in the picker.
    }
}
