mod common;

use crossterm::event::KeyCode;
use spur_tui::action::Action;
use spur_tui::components::input_bar::{EditMode, VimMode};

use common::TestHarness;

#[test]
fn vim_insert_esc_routes_to_input_bar_before_global_handlers() {
    let mut h = TestHarness::new(80, 24);

    spur_tui::test_support::process_action(h.app_mut(), Action::ToggleVimMode);
    h.send_key(KeyCode::Char('i'));
    assert_eq!(
        h.app_mut()
            .dashboard_mut_for_test()
            .input_bar_mut_for_test()
            .mode(),
        EditMode::Vim(VimMode::Insert),
        "setup should put dashboard input bar in Vim Insert mode"
    );

    h.send_key(KeyCode::Esc);

    assert_eq!(
        h.app_mut()
            .dashboard_mut_for_test()
            .input_bar_mut_for_test()
            .mode(),
        EditMode::Vim(VimMode::Normal),
        "Esc in Vim Insert mode should transition to Normal instead of being swallowed globally"
    );
    assert_eq!(
        h.app_mut()
            .dashboard_mut_for_test()
            .input_bar_text_for_test(),
        ""
    );
}
