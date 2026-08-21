use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::config::{ConfigPatch, GRAPH_EMBEDDING_ALIASES};

use crate::action::Action;

const ENV_OVERRIDE_BANNER: &str = "SPUR_EMBEDDING_MODEL overrides this at read time";
const RESTART_HINT: &str = "takes effect on next embedding load (restart)";

pub struct GraphPane {
    aliases: [&'static str; 3],
    selected: usize,
}

impl GraphPane {
    pub fn new(current: Option<&str>) -> Self {
        let aliases = [
            GRAPH_EMBEDDING_ALIASES[0],
            GRAPH_EMBEDDING_ALIASES[1],
            GRAPH_EMBEDDING_ALIASES[2],
        ];
        let selected = current
            .and_then(|alias| aliases.iter().position(|candidate| *candidate == alias))
            .unwrap_or(0);
        Self { aliases, selected }
    }

    pub fn selected_alias(&self) -> &'static str {
        self.aliases[self.selected]
    }

    pub fn cycle(&mut self) {
        self.selected = (self.selected + 1) % self.aliases.len();
    }

    pub fn save_patch(&self) -> ConfigPatch {
        ConfigPatch::GraphEmbeddingModel {
            alias: self.selected_alias().to_string(),
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new(self.body_lines(env_override_set()))
                .block(Block::default().borders(Borders::ALL).title("graph")),
            area,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Enter => {
                self.cycle();
                None
            }
            KeyCode::Char('s') => Some(Action::ConfigSaveRequested {
                patch: self.save_patch(),
            }),
            _ => None,
        }
    }

    fn body_lines(&self, env_override: bool) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = self
            .aliases
            .iter()
            .enumerate()
            .map(|(index, alias)| {
                let marker = if index == self.selected { "> " } else { "  " };
                let style = if index == self.selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![Span::raw(marker), Span::styled(*alias, style)])
            })
            .collect();
        if env_override {
            lines.push(Line::from(""));
            lines.push(Line::from(ENV_OVERRIDE_BANNER));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(RESTART_HINT));
        lines
    }
}

fn env_override_set() -> bool {
    std::env::var("SPUR_EMBEDDING_MODEL").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn body_text(pane: &GraphPane, env_override: bool) -> String {
        pane.body_lines(env_override)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn picker_is_total_over_canonical_aliases() {
        let mut pane = GraphPane::new(None);
        assert_eq!(pane.selected_alias(), "nomic");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "coderank");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "jina-code");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn new_selects_current_alias() {
        let pane = GraphPane::new(Some("jina-code"));
        assert_eq!(pane.selected_alias(), "jina-code");
    }

    #[test]
    fn save_patch_uses_canonical_alias() {
        let pane = GraphPane::new(Some("coderank"));
        match pane.save_patch() {
            spur_acp::config::ConfigPatch::GraphEmbeddingModel { alias } => {
                assert_eq!(alias, "coderank");
            }
            other => panic!("unexpected {other:?}"),
        }

        let pane = GraphPane::new(Some("jina-code"));
        match pane.save_patch() {
            spur_acp::config::ConfigPatch::GraphEmbeddingModel { alias } => {
                assert_eq!(alias, "jina-code");
                assert_ne!(alias, "jina_code");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_current_defaults_to_nomic() {
        let pane = GraphPane::new(Some("jina_code"));
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn save_key_emits_config_save_requested() {
        let mut pane = GraphPane::new(Some("coderank"));
        match pane.handle_key(key(KeyCode::Char('s'))) {
            Some(Action::ConfigSaveRequested { patch }) => match patch {
                ConfigPatch::GraphEmbeddingModel { alias } => {
                    assert_eq!(alias, "coderank");
                }
                other => panic!("unexpected {other:?}"),
            },
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn left_right_enter_cycle_aliases() {
        let mut pane = GraphPane::new(None);
        pane.handle_key(key(KeyCode::Right));
        assert_eq!(pane.selected_alias(), "coderank");
        pane.handle_key(key(KeyCode::Enter));
        assert_eq!(pane.selected_alias(), "jina-code");
        pane.handle_key(key(KeyCode::Left));
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn hint_mentions_restart() {
        let pane = GraphPane::new(None);
        let text = body_text(&pane, false);
        assert!(text.contains(RESTART_HINT));
        assert!(!text.contains(ENV_OVERRIDE_BANNER));
        assert!(text.contains("> nomic"));
        assert!(text.contains("coderank"));
        assert!(text.contains("jina-code"));
    }

    #[test]
    fn env_banner_when_override_set() {
        let pane = GraphPane::new(None);
        let text = body_text(&pane, true);
        assert!(text.contains(ENV_OVERRIDE_BANNER));
        assert!(text.contains(RESTART_HINT));
    }
}
