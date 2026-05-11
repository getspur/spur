#![cfg(feature = "markdown")]

//! Full-screen overlay state for mermaid viewing. Owns only the cursor
//! (which diagram is focused) — the actual `StatefulProtocol` lives in
//! `SessionDetailView::image_cache.overlay` slot.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{layout::Rect, Frame};
use spur_acp::{SessionId, SpurEvent};

use crate::action::Action;
use crate::components::mermaid::{MermaidId, MermaidState};

use super::View;

pub struct MermaidViewerView {
    session_id: SessionId,
    /// Which diagram is currently focused. `None` until `set_available`
    /// chooses a default (the most recent Ready entry).
    pub(crate) focused: Option<MermaidId>,
}

impl MermaidViewerView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            focused: None,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Choose the default focus from the registry. Called by the app
    /// layer before `render_mermaid_overlay`.
    pub fn set_available(&mut self, entries: &[(MermaidId, &MermaidState)]) {
        if self.focused.is_none() {
            self.focused = entries
                .iter()
                .rev()
                .find(|(_, s)| {
                    matches!(
                        s,
                        MermaidState::Ready { .. } | MermaidState::ReadyText { .. }
                    )
                })
                .map(|(id, _)| *id);
        }
    }

    /// Cycle focus among Ready entries.
    pub fn cycle(&mut self, entries: &[(MermaidId, &MermaidState)], forward: bool) {
        let ready_ids: Vec<MermaidId> = entries
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s,
                    MermaidState::Ready { .. } | MermaidState::ReadyText { .. }
                )
            })
            .map(|(id, _)| *id)
            .collect();
        if ready_ids.is_empty() {
            self.focused = None;
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
    }
}

impl View for MermaidViewerView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::NavigateBack),
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {}

    fn render(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &super::ViewContext) {}

    fn tick(&mut self) {}
}
