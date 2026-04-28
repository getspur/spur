use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_core::ExecutorId;
use spur_tui::action::{Action, ViewId};
use spur_tui::views::dashboard::{DashboardMode, Panel};

const PANIC_RESET_HINT: &str = "Returned to Dashboard root";

fn esc(app: &mut spur_tui::app::App) {
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

#[test]
fn triple_esc_within_window_resets_to_dashboard() {
    let mut app = spur_tui::test_support::new_app();

    spur_tui::test_support::process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    app.try_open_palette_for_test();
    spur_tui::test_support::process_action(&mut app, Action::ShowHelp);
    app.dashboard_mut_for_test().handle_paste("draft");
    app.dashboard_mut_for_test()
        .set_focused_node(Some(ExecutorId("worker-1".to_string())));
    app.dashboard_mut_for_test().set_focused_panel(Panel::Log);

    assert!(app.is_palette_visible());
    assert!(app.is_help_visible_for_test());
    assert_eq!(app.current_view_for_test(), &ViewId::IssueBrowser);

    esc(&mut app);
    esc(&mut app);
    esc(&mut app);

    assert!(!app.is_palette_visible());
    assert!(!app.is_help_visible_for_test());
    assert_eq!(app.current_view_for_test(), &ViewId::Dashboard);
    assert_eq!(app.dashboard_for_test().mode(), DashboardMode::Navigate);
    assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Agents);
    assert!(app.dashboard_for_test().focused_node().is_none());
}

#[test]
fn two_esc_then_pause_then_esc_does_not_reset() {
    let mut app = spur_tui::test_support::new_app();

    esc(&mut app);
    esc(&mut app);
    app.age_esc_chain_for_test(Duration::from_millis(1001));
    esc(&mut app);

    assert_eq!(app.esc_chain_len_for_test(), 1);
    assert_ne!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some(PANIC_RESET_HINT)
    );
}

#[test]
fn panic_reset_clears_pickers() {
    let mut app = spur_tui::test_support::new_app();

    esc(&mut app);
    esc(&mut app);
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE));
    assert!(app.dashboard_for_test().completion_active_for_test());

    esc(&mut app);

    assert!(!app.dashboard_for_test().completion_active_for_test());
    assert_eq!(app.current_view_for_test(), &ViewId::Dashboard);
}

#[test]
fn panic_reset_flashes_confirmation_toast() {
    let mut app = spur_tui::test_support::new_app();

    esc(&mut app);
    esc(&mut app);
    esc(&mut app);

    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some(PANIC_RESET_HINT)
    );
}
