use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Paragraph},
    Frame,
};
use spur_pm::Comment;

pub struct IssueCommentsPane {
    scroll_offset: u16,
}

impl IssueCommentsPane {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
    }

    pub fn scroll_up_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(500);
    }

    pub fn render(&self, id: &str, comments: &[Comment], frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Comments: {} ", id))
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let message = if comments.is_empty() {
            "No comments yet".to_string()
        } else {
            format!("{} comments — rendering in next task", comments.len())
        };

        let centered = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        let paragraph = Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray))
            .scroll((self.scroll_offset, 0));
        frame.render_widget(paragraph, centered[1]);
    }
}

impl Default for IssueCommentsPane {
    fn default() -> Self {
        Self::new()
    }
}
