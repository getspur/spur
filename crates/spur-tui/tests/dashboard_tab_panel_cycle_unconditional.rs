use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::views::dashboard::{DashboardView, Panel};
use spur_tui::views::View;

const TAB_DEPRECATION_HINT: &str = "Tab now cycles panels; press Ctrl+E to cycle examples";

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn tab_with_empty_buffer_cycles_panels() {
    let mut app = spur_tui::test_support::new_app();

    assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Log);
    app.handle_crossterm_event_for_test(key(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Agents);
    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some(TAB_DEPRECATION_HINT)
    );

    app.flash_hint_short_for_test("sentinel");
    app.handle_crossterm_event_for_test(key(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Log);
    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some("sentinel"),
        "empty-buffer Tab deprecation hint must be one-shot"
    );
}

#[test]
fn tab_with_non_empty_buffer_still_cycles_panels() {
    let mut app = spur_tui::test_support::new_app();
    app.dashboard_mut_for_test()
        .input_bar_mut_for_test()
        .set_text("draft".to_string(), "draft".len());

    app.handle_crossterm_event_for_test(key(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Agents);
    assert!(
        app.transient_hint_for_test().is_none(),
        "non-empty Tab must not show the empty-input deprecation hint"
    );
}

#[test]
fn ctrl_e_cycles_examples_when_input_empty() {
    let mut dashboard = DashboardView::new();
    let before = dashboard.current_example_prompt_for_test().to_string();

    dashboard.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL), &test_ctx());

    assert_ne!(
        dashboard.current_example_prompt_for_test(),
        before,
        "Ctrl+E must reach the example-cycle path when the input is empty"
    );
    assert_eq!(dashboard.input_bar_text_for_test(), "");
}

#[test]
fn ctrl_e_in_non_empty_input_does_not_cycle_or_type_literal() {
    let mut dashboard = DashboardView::new();
    dashboard.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE), &test_ctx());
    dashboard
        .input_bar_mut_for_test()
        .set_text("draft".to_string(), "draft".len());
    let example_before = dashboard.current_example_prompt_for_test().to_string();

    dashboard.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL), &test_ctx());

    assert_eq!(dashboard.current_example_prompt_for_test(), example_before);
    assert_eq!(
        dashboard.input_bar_text_for_test(),
        "draft",
        "Ctrl+E with non-empty input must stay composer-owned without typing"
    );
}
