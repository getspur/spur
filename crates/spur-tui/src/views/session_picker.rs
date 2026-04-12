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
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
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
        let visible_height = area.height.saturating_sub(4) as usize;

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

        for (i, session) in sessions
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(visible_height)
        {
            let is_selected = i == cursor;
            let prefix = if is_selected { "\u{25b8} " } else { "  " };
            let raw_id = session.session_id.0.as_ref();
            let short_id = &raw_id[..8.min(raw_id.len())];
            let display = Self::display_text(session, show_cwd);
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

        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
        frame.render_widget(Paragraph::new(lines), chunks[0]);
        StatusBar::render(frame, chunks[1], &ViewId::SessionPicker, 0.0, "0m 00s");
    }

    fn render_empty(&self, frame: &mut Frame, area: Rect, agent: &str) {
        let lines = vec![
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
            Line::from(Span::styled(
                "  No saved sessions found.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Start a new conversation from the dashboard.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
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
        let chunks = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
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
                    return None;
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
                        }
                        None
                    }
                    KeyCode::Enter => {
                        let sid = sessions[*cursor].session_id.0.to_string();
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
