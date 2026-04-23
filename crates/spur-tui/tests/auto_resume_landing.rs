use spur_tui::views::session_detail::SessionDetailView;

#[test]
fn banner_visible_immediately_after_show() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess".to_string()),
        "agent".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("agent"),
        Vec::new(),
    );
    assert!(!view.banner_is_visible());
    view.show_resume_banner("my title".into(), "2m ago".into());
    assert!(view.banner_is_visible());
}

#[test]
fn banner_fades_on_any_key_without_consuming() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::components::resume_banner::BannerState;
    use spur_tui::views::View;

    fn test_ctx() -> spur_tui::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        spur_tui::test_support::test_view_ctx(&LINEAGE)
    }

    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess".to_string()),
        "agent".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("agent"),
        Vec::new(),
    );
    view.show_resume_banner("t".into(), "1m".into());
    assert!(view.banner_is_visible());
    let action = view.handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &test_ctx(),
    );
    // Non-mapped key fades the banner without consuming the keystroke.
    assert!(
        matches!(view.banner_state(), Some(BannerState::Fading)),
        "banner should enter Fading after unmapped key, got {:?}",
        view.banner_state()
    );
    assert!(
        action.is_none(),
        "unmapped key must not produce an action, got {:?}",
        action
    );
}
