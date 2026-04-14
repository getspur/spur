use spur_tui::views::session_detail::SessionDetailView;

#[test]
fn banner_visible_immediately_after_show() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess".to_string()),
        "agent".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("agent"),
    );
    assert!(!view.banner_is_visible());
    view.show_resume_banner("my title".into(), "2m ago".into());
    assert!(view.banner_is_visible());
}

#[test]
fn banner_dismisses_on_any_key_without_consuming() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::views::View;

    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess".to_string()),
        "agent".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("agent"),
    );
    view.show_resume_banner("t".into(), "1m".into());
    assert!(view.banner_is_visible());
    let _ = view.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(!view.banner_is_visible());
}
