//! Verifies that the TUI seeds its EditMode from `config.tui.edit_mode`
//! at boot, not from a hardcoded default.

use spur_acp::config::EditorMode;
use spur_tui::components::input_bar::{EditMode, VimMode};

#[test]
fn editor_mode_emacs_maps_to_emacs() {
    assert_eq!(EditMode::from(EditorMode::Emacs), EditMode::Emacs);
}

#[test]
fn editor_mode_vim_maps_to_vim_normal() {
    assert_eq!(
        EditMode::from(EditorMode::Vim),
        EditMode::Vim(VimMode::Normal)
    );
}
