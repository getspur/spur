use spur_tui::action::Action;

#[test]
fn mode_toggle_action_exists() {
    // Compiles ⇒ the variant exists. End-to-end wiring is smoke-tested manually.
    let _ = Action::TogglePlanMode;
}
