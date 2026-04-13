#![cfg(feature = "markdown")]

//! Full-screen overlay that renders a single mermaid diagram from the
//! active session's registry via `ratatui-image`'s `StatefulImage`.
//!
//! Because `View::render` takes `&self` but `StatefulImage` needs
//! `&mut StatefulProtocol`, the actual image draw happens in a helper
//! in `app.rs` (Task 10) that has mutable access. This file owns the
//! overlay's cursor state (which diagram is focused) and the protocol
//! once built by the app.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{layout::Rect, Frame};
use ratatui_image::protocol::StatefulProtocol;
use spur_acp::{SessionId, SpurEvent};

use crate::action::Action;
use crate::components::mermaid::{MermaidId, MermaidState};

use super::View;

pub struct MermaidViewerView {
    session_id: SessionId,
    /// Which diagram is currently focused. `None` until set_available
    /// chooses a default (the most recent Ready entry).
    pub(crate) focused: Option<MermaidId>,
    /// Lazily-built protocol, bound to the currently focused image.
    /// Populated by `set_available` when the app layer supplies a Picker.
    pub(crate) protocol: Option<StatefulProtocol>,
}

impl MermaidViewerView {
    pub fn new(session_id: SessionId) -> Self {
        Self { session_id, focused: None, protocol: None }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Called by the app layer before `render_mermaid_overlay`. Supplies
    /// the registry entries (cloned) and the Picker. Chooses default
    /// focus if none set, and rebuilds protocol if needed.
    pub fn set_available(
        &mut self,
        entries: &[(MermaidId, &MermaidState)],
        picker: Option<&ratatui_image::picker::Picker>,
    ) {
        if self.focused.is_none() {
            self.focused = entries
                .iter()
                .rev()
                .find(|(_, s)| matches!(s, MermaidState::Ready { .. }))
                .map(|(id, _)| *id);
        }
        if let (Some(id), Some(picker)) = (self.focused, picker) {
            if self.protocol.is_none() {
                if let Some(state) = entries.iter().find(|(i, _)| *i == id).map(|(_, s)| *s) {
                    if let MermaidState::Ready { image } = state {
                        self.protocol = Some(picker.new_resize_protocol(image.clone()));
                    }
                }
            }
        }
    }

    /// Cycle focus among Ready entries. `forward=true` moves to next;
    /// `false` moves to previous. Drops the current protocol so
    /// `set_available` rebuilds next frame.
    pub fn cycle(&mut self, entries: &[(MermaidId, &MermaidState)], forward: bool) {
        let ready_ids: Vec<MermaidId> = entries
            .iter()
            .filter(|(_, s)| matches!(s, MermaidState::Ready { .. }))
            .map(|(id, _)| *id)
            .collect();
        if ready_ids.is_empty() {
            self.focused = None;
            self.protocol = None;
            return;
        }
        let idx = self
            .focused
            .and_then(|cur| ready_ids.iter().position(|i| *i == cur))
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % ready_ids.len()
        } else {
            (idx + ready_ids.len() - 1) % ready_ids.len()
        };
        self.focused = Some(ready_ids[next]);
        self.protocol = None;
    }

    pub(crate) fn protocol_mut(&mut self) -> Option<&mut StatefulProtocol> {
        self.protocol.as_mut()
    }
}

impl View for MermaidViewerView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::NavigateBack),
            // `[` and `]` are dispatched at the app level (which can pass
            // in the registry entries). Returning None here lets the app
            // layer handle the cycle.
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent) {}

    fn render(&self, _frame: &mut Frame, _area: Rect) {
        // Intentionally empty. The real draw is in
        // `app::render_mermaid_overlay` which has mutable access to
        // `self.protocol`. This stub satisfies the View trait.
    }

    fn tick(&mut self) {}
}
