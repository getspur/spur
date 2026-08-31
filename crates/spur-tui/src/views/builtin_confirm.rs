use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use spur_acp::config::{BuiltinMcpServer, ConfigPatch};

use crate::action::Action;

const TITLE: &str = "Disable spur-mcp?";
const WARNING: &str =
    "This removes delegation/plan/solve tools from brain sessions and applies to next session.";
const KEYS: &str = "Enter/y confirm  Esc/q/n cancel";

/// Confirmation pane guarding the destructive `spur-mcp` disable toggle.
#[derive(Debug, Default)]
pub(crate) struct BuiltinConfirmPane {
    open: bool,
}

impl BuiltinConfirmPane {
    /// Opens the confirmation prompt.
    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    /// Returns whether the prompt currently owns keyboard input.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Handles one prompt key, emitting the guarded config patch only on confirm.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                self.open = false;
                Some(Action::ConfigSaveRequested {
                    patch: ConfigPatch::BuiltinMcpToggle {
                        server: BuiltinMcpServer::SpurMcp,
                        enabled: false,
                    },
                })
            }
            KeyCode::Esc | KeyCode::Char('q' | 'n') => {
                self.open = false;
                None
            }
            _ => None,
        }
    }

    /// Renders the prompt over `area` when open.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![WARNING.into(), "".into(), KEYS.into()])
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(TITLE)),
            area,
        );
    }

    /// Plain-text representation used by pane snapshots.
    pub(crate) fn snapshot_rows(&self) -> Vec<String> {
        vec![TITLE.into(), WARNING.into(), KEYS.into()]
    }
}
