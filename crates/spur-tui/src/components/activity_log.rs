use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::{LogEntry, LogEntryKind, MAX_LOG_ENTRIES, focused_border_style};

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
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_bottom(following_indicator)
            .borders(Borders::ALL)
            .border_style(focused_border_style(self.focused));

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
