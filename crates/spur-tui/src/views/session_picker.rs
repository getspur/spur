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
        };
        self.scroll_offset.set(0);
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
    ) {
        let show_cwd = Self::cwds_are_heterogeneous(sessions);
        let visible_height = area.height.saturating_sub(4) as usize;

        // Clamp scroll_offset so cursor is always visible.
        // `cursor` indexes a virtual list where 0 = [+ New session] row and
        // 1..=sessions.len() are real sessions. We scroll the real sessions;
        // the [+ New session] row is always visible as the first entry.
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

        for (i, session) in sessions
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_height)
        {
            let is_selected = cursor == i + 1;
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
        match &mut self.state {
            PickerState::Populated {
                sessions,
                cursor,
                resuming,
                ..
            } => {
                if *resuming {
                    return None;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *cursor > 0 {
                            *cursor -= 1;
                        }
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *cursor < sessions.len() {
                            *cursor += 1;
                        }
                        None
                    }
                    KeyCode::Char('n') => Some(Action::NewSessionRequested),
                    KeyCode::Enter => {
                        if *cursor == 0 {
                            Some(Action::NewSessionRequested)
                        } else {
                            let sid = sessions[*cursor - 1].session_id.0.to_string();
                            *resuming = true;
                            Some(Action::ResumeSession { session_id: sid })
                        }
                    }
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
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
            } => self.render_populated(frame, area, agent, sessions, *cursor, *resuming),
            PickerState::Error { message } => self.render_error(frame, area, message),
        }
    }

    fn tick(&mut self) {
        // No animations in the picker.
    }
}
