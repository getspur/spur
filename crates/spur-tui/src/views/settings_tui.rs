use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::action::Action;

pub struct TuiPane;

impl TuiPane {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new("tui").block(Block::default().borders(Borders::ALL).title("tui")),
            area,
        );
    }

    pub fn handle_key(&mut self, _key: KeyEvent) -> Option<Action> {
        None
    }
}

impl Default for TuiPane {
    fn default() -> Self {
        Self::new()
    }
}
