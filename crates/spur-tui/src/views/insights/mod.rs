//! Experimental Insights view (analytics feature).
//!
//! Module skeleton; full implementation lands in C.4-C.9.

pub mod builder;
pub mod refresh;
pub mod state;
pub mod tabs;
pub mod widgets;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};
use spur_acp::SpurEvent;

use crate::action::Action;

use super::{View, ViewContext};

pub struct InsightsView;

impl InsightsView {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InsightsView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for InsightsView {
    fn handle_key(&mut self, _key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
        None
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &ViewContext) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
        // Phase 1 placeholder — actual rendering wired up in C.6.
        use ratatui::widgets::Paragraph;

        let p = Paragraph::new("Insights view (analytics) — module skeleton (C.3)");
        frame.render_widget(p, area);
    }

    fn tick(&mut self) {}
}
