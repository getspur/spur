pub mod dashboard;
#[cfg(feature = "markdown")]
pub mod mermaid_viewer;
pub mod session_detail;
pub mod session_picker;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use spur_acp::SpurEvent;

use crate::action::Action;

/// Trait for top-level views (Dashboard, Session Detail, etc.).
pub trait View {
    /// Handle a keyboard event. Return an Action if the view wants the app to do something.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;
    /// Process an orchestrator event, updating internal state.
    fn handle_spur_event(&mut self, event: &SpurEvent);
    /// Render the view into the given frame area.
    fn render(&self, frame: &mut Frame, area: Rect);
    /// Called on each tick (for spinner animations, batched text flush, etc.).
    fn tick(&mut self);
}
