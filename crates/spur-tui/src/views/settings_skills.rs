use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::config::{ConfigPatch, SkillsProjectionMode};

use crate::action::Action;

const MODES: [SkillsProjectionMode; 2] = [
    SkillsProjectionMode::CatalogOnly,
    SkillsProjectionMode::AllActive,
];

const APPLY_HINT: &str = "applies to newly reconciled sessions";
const ALL_ACTIVE_CONSEQUENCE: &str =
    "projects every bundled and accepted pool skill (large context)";

pub struct SkillsPane {
    mode: SkillsProjectionMode,
}

impl SkillsPane {
    pub fn new() -> Self {
        Self {
            mode: SkillsProjectionMode::CatalogOnly,
        }
    }

    pub fn cycle(&mut self) {
        self.mode = match self.mode {
            SkillsProjectionMode::CatalogOnly => SkillsProjectionMode::AllActive,
            SkillsProjectionMode::AllActive => SkillsProjectionMode::CatalogOnly,
        };
    }

    pub fn save_patch(&self) -> ConfigPatch {
        ConfigPatch::SkillsProjectionMode(self.mode)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new(self.render_lines())
                .block(Block::default().borders(Borders::ALL).title("skills")),
            area,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                self.cycle();
                None
            }
            KeyCode::Char('s') => Some(Action::ConfigSaveRequested {
                patch: self.save_patch(),
            }),
            _ => None,
        }
    }

    fn mode_label(mode: SkillsProjectionMode) -> &'static str {
        match mode {
            SkillsProjectionMode::CatalogOnly => "catalog_only",
            SkillsProjectionMode::AllActive => "all_active",
        }
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = MODES
            .iter()
            .copied()
            .map(|mode| {
                let selected = mode == self.mode;
                let marker = if selected { "> " } else { "  " };
                let style = if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::raw(marker),
                    Span::styled(Self::mode_label(mode), style),
                ])
            })
            .collect();
        if self.mode == SkillsProjectionMode::AllActive {
            lines.push(Line::from(ALL_ACTIVE_CONSEQUENCE));
        }
        lines.push(Line::from(APPLY_HINT));
        lines
    }

    #[cfg(test)]
    fn content_lines(&self) -> Vec<String> {
        self.render_lines()
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }
}

impl Default for SkillsPane {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::config::{ConfigPatch, SkillsProjectionMode};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn assert_mode(patch: ConfigPatch, expected: SkillsProjectionMode) {
        match patch {
            ConfigPatch::SkillsProjectionMode(mode) => assert_eq!(mode, expected),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mode_cycles_catalog_only_all_active() {
        let mut pane = SkillsPane::new();
        assert_mode(pane.save_patch(), SkillsProjectionMode::CatalogOnly);
        pane.cycle();
        match pane.save_patch() {
            ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::AllActive) => {}
            other => panic!("{other:?}"),
        }
        pane.cycle();
        match pane.save_patch() {
            ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::CatalogOnly) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn save_emits_skills_projection_patch() {
        let mut pane = SkillsPane::new();
        pane.cycle();
        match pane.handle_key(key(KeyCode::Char('s'))) {
            Some(Action::ConfigSaveRequested { patch }) => {
                assert_mode(patch, SkillsProjectionMode::AllActive);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn left_right_space_cycle_mode() {
        let mut pane = SkillsPane::new();
        assert!(pane.handle_key(key(KeyCode::Right)).is_none());
        assert_mode(pane.save_patch(), SkillsProjectionMode::AllActive);
        assert!(pane.handle_key(key(KeyCode::Left)).is_none());
        assert_mode(pane.save_patch(), SkillsProjectionMode::CatalogOnly);
        assert!(pane.handle_key(key(KeyCode::Char(' '))).is_none());
        assert_mode(pane.save_patch(), SkillsProjectionMode::AllActive);
    }

    #[test]
    fn labels_are_serde_snake_case() {
        let pane = SkillsPane::new();
        let lines = pane.content_lines();
        assert!(lines.iter().any(|line| line.contains("catalog_only")));
        assert!(lines.iter().any(|line| line.contains("all_active")));
        assert!(!lines.iter().any(|line| line.contains("CatalogOnly")));
        assert!(!lines.iter().any(|line| line.contains("AllActive")));
    }

    #[test]
    fn hint_applies_to_newly_reconciled_sessions() {
        let mut pane = SkillsPane::new();
        assert!(pane
            .content_lines()
            .iter()
            .any(|line| line.contains("applies to newly reconciled sessions")));
        pane.cycle();
        assert!(pane
            .content_lines()
            .iter()
            .any(|line| line.contains("applies to newly reconciled sessions")));
    }

    #[test]
    fn all_active_shows_larger_projected_skill_set_line() {
        let mut pane = SkillsPane::new();
        assert!(!pane.content_lines().iter().any(|line| {
            line.contains("projects every bundled and accepted pool skill (large context)")
        }));
        pane.cycle();
        assert!(pane.content_lines().iter().any(|line| {
            line.contains("projects every bundled and accepted pool skill (large context)")
        }));
    }
}
