use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_acp::config::{ConfigPatch, EditorMode};

use crate::action::Action;
#[cfg(test)]
use crate::theme::BUILT_IN_THEME_NAMES;
use crate::theme::{list_available_themes, AvailableThemes};

const FIELD_COUNT: usize = 3;
const FIELD_EDIT_MODE: usize = 0;
const FIELD_THEME: usize = 1;
const FIELD_PASTE_BURST: usize = 2;

pub struct TuiPane {
    edit_mode: EditorMode,
    theme: String,
    themes: Vec<String>,
    disable_paste_burst: bool,
    selected_field: usize,
}

impl TuiPane {
    /// Zero-arg constructor required by the `/configure` shell (`TuiPane::new()`).
    pub fn new() -> Self {
        Self::from_prefs(EditorMode::Emacs, "dark".into(), false)
    }

    /// Load discovered themes. Unknown `theme` stays selected but is not saved.
    pub fn from_prefs(edit_mode: EditorMode, theme: String, disable_paste_burst: bool) -> Self {
        Self::with_themes(
            edit_mode,
            theme,
            flatten_theme_names(list_available_themes()),
            disable_paste_burst,
        )
    }

    fn with_themes(
        edit_mode: EditorMode,
        theme: String,
        themes: Vec<String>,
        disable_paste_burst: bool,
    ) -> Self {
        Self {
            edit_mode,
            theme,
            themes,
            disable_paste_burst,
            selected_field: FIELD_EDIT_MODE,
        }
    }

    #[cfg(test)]
    pub fn new_for_test(edit_mode: EditorMode, theme: String, disable_paste_burst: bool) -> Self {
        let themes: Vec<String> = BUILT_IN_THEME_NAMES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        Self::with_themes(edit_mode, theme, themes, disable_paste_burst)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let mode = match self.edit_mode {
            EditorMode::Emacs => "emacs",
            EditorMode::Vim => "vim",
        };
        let theme_value = if self.theme_save_allowed() {
            self.theme.clone()
        } else {
            format!("{} (unknown)", self.theme)
        };
        let paste = if self.disable_paste_burst {
            "true"
        } else {
            "false"
        };
        let rows = [
            format!("edit_mode: {mode}"),
            format!("theme: {theme_value}"),
            format!("disable_paste_burst: {paste}"),
        ];
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .map(|(i, row)| {
                let prefix = if i == self.selected_field { "> " } else { "  " };
                Line::from(format!("{prefix}{row}"))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("TUI")),
            area,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_field(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_field(1);
                None
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char(' ')
            | KeyCode::Char('e')
            | KeyCode::Enter => {
                self.cycle_focused();
                None
            }
            KeyCode::Char('s') => self.save_focused(),
            _ => None,
        }
    }

    pub fn cycle_edit_mode(&mut self) {
        self.edit_mode = match self.edit_mode {
            EditorMode::Emacs => EditorMode::Vim,
            EditorMode::Vim => EditorMode::Emacs,
        };
    }

    pub fn toggle_paste_burst(&mut self) {
        self.disable_paste_burst = !self.disable_paste_burst;
    }

    pub fn cycle_theme(&mut self) {
        if self.themes.is_empty() {
            return;
        }
        if let Some(idx) = self.themes.iter().position(|name| name == &self.theme) {
            self.theme = self.themes[(idx + 1) % self.themes.len()].clone();
        } else {
            self.theme = self.themes[0].clone();
        }
    }

    pub fn theme_save_allowed(&self) -> bool {
        self.themes.iter().any(|name| name == &self.theme)
    }

    pub fn edit_mode_patch(&self) -> ConfigPatch {
        ConfigPatch::TuiEditMode(self.edit_mode)
    }

    pub fn theme_patch(&self) -> ConfigPatch {
        ConfigPatch::TuiTheme(self.theme.clone())
    }

    pub fn paste_burst_patch(&self) -> ConfigPatch {
        ConfigPatch::TuiDisablePasteBurst(self.disable_paste_burst)
    }

    fn move_field(&mut self, delta: isize) {
        let n = FIELD_COUNT as isize;
        self.selected_field = (self.selected_field as isize + delta).rem_euclid(n) as usize;
    }

    fn cycle_focused(&mut self) {
        match self.selected_field {
            FIELD_EDIT_MODE => self.cycle_edit_mode(),
            FIELD_THEME => self.cycle_theme(),
            FIELD_PASTE_BURST => self.toggle_paste_burst(),
            _ => {}
        }
    }

    fn save_focused(&self) -> Option<Action> {
        let patch = match self.selected_field {
            FIELD_EDIT_MODE => self.edit_mode_patch(),
            FIELD_THEME => {
                if !self.theme_save_allowed() {
                    return None;
                }
                self.theme_patch()
            }
            FIELD_PASTE_BURST => self.paste_burst_patch(),
            _ => return None,
        };
        Some(Action::ConfigSaveRequested { patch })
    }
}

impl Default for TuiPane {
    fn default() -> Self {
        Self::new()
    }
}

fn flatten_theme_names(available: AvailableThemes) -> Vec<String> {
    let mut names = Vec::new();
    for name in available
        .built_in
        .into_iter()
        .chain(available.project)
        .chain(available.user)
    {
        if !names.iter().any(|existing| existing == &name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn edit_mode_cycles_emacs_vim() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        assert!(matches!(
            pane.edit_mode_patch(),
            ConfigPatch::TuiEditMode(EditorMode::Emacs)
        ));
        pane.cycle_edit_mode();
        assert!(matches!(
            pane.edit_mode_patch(),
            ConfigPatch::TuiEditMode(EditorMode::Vim)
        ));
        pane.cycle_edit_mode();
        assert!(matches!(
            pane.edit_mode_patch(),
            ConfigPatch::TuiEditMode(EditorMode::Emacs)
        ));
    }

    #[test]
    fn paste_burst_toggle() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        pane.toggle_paste_burst();
        assert!(matches!(
            pane.paste_burst_patch(),
            ConfigPatch::TuiDisablePasteBurst(true)
        ));
    }

    #[test]
    fn theme_patch_uses_selected_name() {
        let pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "dark"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn theme_cycles_discovered_names() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        pane.cycle_theme();
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "light"),
            other => panic!("{other:?}"),
        }
        pane.cycle_theme();
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "high-contrast"),
            other => panic!("{other:?}"),
        }
        pane.cycle_theme();
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "dark"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_theme_is_not_saved() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "not-a-theme".into(), false);
        assert!(!pane.theme_save_allowed());
        pane.handle_key(key(KeyCode::Down));
        assert!(pane.handle_key(key(KeyCode::Char('s'))).is_none());
    }

    #[test]
    fn cycling_unknown_theme_lands_on_discovered_name() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "not-a-theme".into(), false);
        pane.cycle_theme();
        assert!(pane.theme_save_allowed());
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "dark"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn save_emits_only_the_focused_field_patch() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        match pane.handle_key(key(KeyCode::Char('s'))) {
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::TuiEditMode(EditorMode::Emacs),
            }) => {}
            other => panic!("expected edit-mode patch, got {other:?}"),
        }

        pane.handle_key(key(KeyCode::Down));
        match pane.handle_key(key(KeyCode::Char('s'))) {
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::TuiTheme(name),
            }) => assert_eq!(name, "dark"),
            other => panic!("expected theme patch, got {other:?}"),
        }

        pane.handle_key(key(KeyCode::Down));
        match pane.handle_key(key(KeyCode::Char('s'))) {
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::TuiDisablePasteBurst(false),
            }) => {}
            other => panic!("expected paste-burst patch, got {other:?}"),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
}
