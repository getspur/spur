use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::action::Action;

pub struct GraphPane;

impl GraphPane {
    pub fn new(_current: Option<&str>) -> Self {
        Self
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new("graph").block(Block::default().borders(Borders::ALL).title("graph")),
            area,
        );
    }

    pub fn handle_key(&mut self, _key: KeyEvent) -> Option<Action> {
        None
    }
}
