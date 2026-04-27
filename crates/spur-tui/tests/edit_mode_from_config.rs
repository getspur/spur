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

/// Boot integration: a vim-configured user must see Vim(Normal) on the
/// dashboard's input bar at startup. Regression guard for the gap caught
/// in kimi's T3 review — `App::new` was seeding `App.edit_mode` from
/// config but not propagating to `DashboardView::input_bar`, which
/// `InputBar::new()` hardcodes to `EditMode::Emacs`.
#[test]
fn app_boots_dashboard_input_bar_in_vim_when_config_says_vim() {
    let mut config = spur_acp::SpurConfig::default();
    config.tui.edit_mode = EditorMode::Vim;
    let mut app = spur_tui::app::App::new_with_config(
        None,
        false,
        std::sync::Arc::new(config),
        spur_tui::landing::LandingDecision::ShowDashboard,
    );
    let dashboard_mode = app.dashboard_mut_for_test().input_bar_mut_for_test().mode();
    assert!(
        matches!(dashboard_mode, EditMode::Vim(_)),
        "expected Vim(_), got {dashboard_mode:?}"
    );
}

#[test]
fn app_boots_dashboard_input_bar_in_emacs_when_config_says_emacs() {
    let config = spur_acp::SpurConfig::default(); // tui.edit_mode = Emacs
    let mut app = spur_tui::app::App::new_with_config(
        None,
        false,
        std::sync::Arc::new(config),
        spur_tui::landing::LandingDecision::ShowDashboard,
    );
    let dashboard_mode = app.dashboard_mut_for_test().input_bar_mut_for_test().mode();
    assert_eq!(dashboard_mode, EditMode::Emacs);
}
