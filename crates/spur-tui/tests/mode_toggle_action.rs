use spur_tui::action::Action;

#[test]
fn set_session_mode_action_exists() {
    // Compiles ⇒ the capability-derived mode action remains public.
    let _ = Action::SetSessionMode {
        mode_id: "agent".into(),
    };
}
