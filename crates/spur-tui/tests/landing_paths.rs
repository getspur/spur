//! Integration tests for TUI landing decision paths.
//!
//! Verifies that each LandingDecision variant produces the correct
//! initial App state: view selection, dashboard configuration, and
//! (where applicable) banner readiness.

use spur_tui::app::App;
use spur_tui::landing::LandingDecision;

#[test]
fn landing_setup_required_shows_nudge() {
    let app = App::new_with_config(
        None,
        false,
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        LandingDecision::SetupRequired,
    );
    assert!(!app.dashboard_is_configured());
    assert_eq!(*app.current_view(), spur_tui::action::ViewId::Dashboard);
}

#[test]
fn landing_show_dashboard_is_default() {
    let app = App::new_with_config(
        None,
        false,
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        LandingDecision::ShowDashboard,
    );
    assert!(app.dashboard_is_configured());
    assert_eq!(*app.current_view(), spur_tui::action::ViewId::Dashboard);
}

#[test]
fn landing_show_picker_opens_picker() {
    let app = App::new_with_config(
        None,
        true, // start_in_picker
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        LandingDecision::ShowPicker,
    );
    assert_eq!(*app.current_view(), spur_tui::action::ViewId::SessionPicker);
}

#[test]
fn landing_auto_resume_defaults_to_dashboard_before_spawn() {
    // AutoResume landing starts in Dashboard view; the SessionDetail
    // (and resume banner) are created later when BrainSpawned fires.
    let app = App::new_with_config(
        None,
        false,
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        LandingDecision::AutoResume {
            acp_id: "acp-123".into(),
            brain: "claude-code".into(),
        },
    );
    assert!(app.dashboard_is_configured());
    assert_eq!(*app.current_view(), spur_tui::action::ViewId::Dashboard);
}
