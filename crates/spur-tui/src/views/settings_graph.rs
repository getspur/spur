use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::config::{ConfigPatch, OverlayFsmonitorMode, GRAPH_EMBEDDING_ALIASES};

use crate::action::Action;

const ENV_OVERRIDE_BANNER: &str = "SPUR_EMBEDDING_MODEL overrides this at read time";
const RESTART_HINT: &str = "takes effect on next embedding load (restart)";
const OVERLAY_FSMONITOR_HINT: &str = "Auto is experimental for local repositories; unsupported or unhealthy environments use exact fallback; restart required";

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphRow {
    EmbeddingModel,
    OverlayFsmonitor,
}

pub struct GraphPane {
    aliases: [&'static str; 3],
    selected: usize,
    overlay_fsmonitor: OverlayFsmonitorMode,
    selected_row: GraphRow,
}

impl GraphPane {
    pub fn new(current: Option<&str>, overlay_fsmonitor: OverlayFsmonitorMode) -> Self {
        let aliases = [
            GRAPH_EMBEDDING_ALIASES[0],
            GRAPH_EMBEDDING_ALIASES[1],
            GRAPH_EMBEDDING_ALIASES[2],
        ];
        let selected = current
            .and_then(|alias| aliases.iter().position(|candidate| *candidate == alias))
            .unwrap_or(0);
        Self {
            aliases,
            selected,
            overlay_fsmonitor,
            selected_row: GraphRow::EmbeddingModel,
        }
    }

    pub fn selected_alias(&self) -> &'static str {
        self.aliases[self.selected]
    }

    pub fn cycle(&mut self) {
        self.shift_choice(1);
    }

    fn shift_choice(&mut self, delta: isize) {
        match self.selected_row {
            GraphRow::EmbeddingModel => {
                let n = self.aliases.len() as isize;
                self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
            }
            GraphRow::OverlayFsmonitor => {
                self.overlay_fsmonitor = match self.overlay_fsmonitor {
                    OverlayFsmonitorMode::Off => OverlayFsmonitorMode::Auto,
                    OverlayFsmonitorMode::Auto => OverlayFsmonitorMode::Off,
                };
            }
        }
    }

    pub fn save_patch(&self) -> ConfigPatch {
        match self.selected_row {
            GraphRow::EmbeddingModel => ConfigPatch::GraphEmbeddingModel {
                alias: self.selected_alias().to_string(),
            },
            GraphRow::OverlayFsmonitor => {
                ConfigPatch::GraphOverlayFsmonitor(self.overlay_fsmonitor)
            }
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new(self.body_lines(env_override_set()))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Graph")),
            area,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Right | KeyCode::Enter => {
                self.shift_choice(1);
                None
            }
            KeyCode::Left => {
                self.shift_choice(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_row = GraphRow::OverlayFsmonitor;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_row = GraphRow::EmbeddingModel;
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
                let marker =
                    if self.selected_row == GraphRow::EmbeddingModel && index == self.selected {
                        "> "
                    } else {
                        "  "
                    };
                let style = if index == self.selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![Span::raw(marker), Span::styled(*alias, style)])
            })
            .collect();
        lines.push(Line::from(""));
        let marker = if self.selected_row == GraphRow::OverlayFsmonitor {
            "> "
        } else {
            "  "
        };
        let off_style = if self.overlay_fsmonitor == OverlayFsmonitorMode::Off {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let auto_style = if self.overlay_fsmonitor == OverlayFsmonitorMode::Auto {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::raw("Overlay fsmonitor: "),
            Span::styled(OverlayFsmonitorMode::Off.to_string(), off_style),
            Span::raw(" | "),
            Span::styled(OverlayFsmonitorMode::Auto.to_string(), auto_style),
        ]));
        lines.push(Line::from(OVERLAY_FSMONITOR_HINT));
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
    use spur_acp::config::OverlayFsmonitorMode;

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
        let mut pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
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
        let pane = GraphPane::new(Some("jina-code"), OverlayFsmonitorMode::Off);
        assert_eq!(pane.selected_alias(), "jina-code");
    }

    #[test]
    fn save_patch_uses_canonical_alias() {
        let pane = GraphPane::new(Some("coderank"), OverlayFsmonitorMode::Off);
        match pane.save_patch() {
            spur_acp::config::ConfigPatch::GraphEmbeddingModel { alias } => {
                assert_eq!(alias, "coderank");
            }
            other => panic!("unexpected {other:?}"),
        }

        let pane = GraphPane::new(Some("jina-code"), OverlayFsmonitorMode::Off);
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
        let pane = GraphPane::new(Some("jina_code"), OverlayFsmonitorMode::Off);
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn save_key_emits_config_save_requested() {
        let mut pane = GraphPane::new(Some("coderank"), OverlayFsmonitorMode::Off);
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
        let mut pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
        pane.handle_key(key(KeyCode::Right));
        assert_eq!(pane.selected_alias(), "coderank");
        pane.handle_key(key(KeyCode::Enter));
        assert_eq!(pane.selected_alias(), "jina-code");
        pane.handle_key(key(KeyCode::Left));
        assert_eq!(pane.selected_alias(), "coderank");
        pane.handle_key(key(KeyCode::Left));
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn hint_mentions_restart() {
        let pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
        let text = body_text(&pane, false);
        assert!(text.contains(RESTART_HINT));
        assert!(!text.contains(ENV_OVERRIDE_BANNER));
        assert!(text.contains("> nomic"));
        assert!(text.contains("coderank"));
        assert!(text.contains("jina-code"));
    }

    #[test]
    fn env_banner_when_override_set() {
        let pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
        let text = body_text(&pane, true);
        assert!(text.contains(ENV_OVERRIDE_BANNER));
        assert!(text.contains(RESTART_HINT));
    }

    #[test]
    fn graph_pane_initializes_overlay_fsmonitor_auto() {
        let mut pane = GraphPane::new(None, OverlayFsmonitorMode::Auto);
        pane.handle_key(key(KeyCode::Down));

        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
        ));
    }

    #[test]
    fn graph_pane_up_down_focuses_separate_rows() {
        let mut pane = GraphPane::new(Some("coderank"), OverlayFsmonitorMode::Off);

        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphEmbeddingModel { ref alias } if alias == "coderank"
        ));
        pane.handle_key(key(KeyCode::Down));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Off)
        ));
        pane.handle_key(key(KeyCode::Up));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphEmbeddingModel { ref alias } if alias == "coderank"
        ));
    }

    #[test]
    fn graph_pane_left_right_enter_cycle_only_off_and_auto() {
        let mut pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
        pane.handle_key(key(KeyCode::Down));

        pane.handle_key(key(KeyCode::Right));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
        ));
        pane.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Off)
        ));
        pane.handle_key(key(KeyCode::Left));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
        ));
    }

    #[test]
    fn graph_pane_saves_auto_without_changing_embedding() {
        let mut pane = GraphPane::new(Some("coderank"), OverlayFsmonitorMode::Auto);
        pane.handle_key(key(KeyCode::Down));

        assert!(matches!(
            pane.handle_key(key(KeyCode::Char('s'))),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
            })
        ));

        pane.handle_key(key(KeyCode::Up));
        assert!(matches!(
            pane.save_patch(),
            ConfigPatch::GraphEmbeddingModel { ref alias } if alias == "coderank"
        ));
    }

    #[test]
    fn graph_pane_copy_describes_experimental_exact_fallback_and_restart() {
        let pane = GraphPane::new(None, OverlayFsmonitorMode::Off);
        let text = body_text(&pane, false);

        assert!(text.contains("Auto (experimental)"));
        assert!(text.contains("local repositories"));
        assert!(text.contains("exact fallback"));
        assert!(text.contains("restart required"));
    }
}
