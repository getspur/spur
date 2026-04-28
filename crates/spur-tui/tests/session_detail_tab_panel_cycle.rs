use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::views::{
    session_detail::{FocusedSessionPanel, SessionDetailView},
    View,
};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn mk_view() -> SessionDetailView {
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude"),
        Vec::new(),
    )
}

fn mk_view_in_cwd(cwd: std::path::PathBuf) -> SessionDetailView {
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        cwd,
        spur_tui::test_support::default_agent_config("claude"),
        Vec::new(),
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn press_mod(v: &mut SessionDetailView, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, modifiers), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

#[test]
fn tab_cycles_panels_when_input_empty_no_picker() {
    let mut detail = mk_view();
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);

    let action = press(&mut detail, KeyCode::Tab);

    assert!(matches!(action, Some(Action::CycleFocus)));
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::Workers);

    let action = press(&mut detail, KeyCode::Tab);

    assert!(matches!(action, Some(Action::CycleFocus)));
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
}

#[test]
fn shift_tab_reverses_cycle() {
    let mut detail = mk_view();
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);

    let action = press_mod(&mut detail, KeyCode::BackTab, KeyModifiers::SHIFT);

    assert!(matches!(action, Some(Action::CycleFocus)));
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::Workers);
}

#[test]
fn tab_with_non_empty_input_flows_to_composer() {
    let mut detail = mk_view();
    detail.input_bar_mut_for_test().set_text("hello".into(), 5);

    let action = press(&mut detail, KeyCode::Tab);

    assert!(action.is_none());
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
    let text = detail.input_bar_text_for_test();
    assert!(text.starts_with("hello"));
    assert_ne!(text, "hello", "composer must receive and apply Tab");
}

#[test]
fn tab_with_active_picker_flows_to_picker() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut detail = mk_view_in_cwd(tmp.path().to_path_buf());

    type_str(&mut detail, "@NOT");
    assert!(detail.completion_active_for_test());

    let action = press(&mut detail, KeyCode::Tab);

    assert!(action.is_none());
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
    assert!(!detail.completion_active_for_test());
    let action = press(&mut detail, KeyCode::Enter).expect("submit action after picker accept");
    match action {
        Action::SendMessage { blocks, .. } => {
            assert!(
                blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ResourceLink(_))),
                "expected picker Tab to accept a ResourceLink, got {blocks:?}"
            );
        }
        other => panic!("expected SendMessage, got {other:?}"),
    }
}

#[test]
fn tab_with_history_shell_flows_to_shell() {
    let mut detail = mk_view();
    detail
        .input_bar_mut_for_test()
        .seed_history(vec![InputHistoryEntry::new(InputStateSnapshot::from_text(
            "previous prompt",
        ))]);

    press_mod(&mut detail, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(detail.completion_active_for_test());

    let action = press(&mut detail, KeyCode::Tab);

    assert!(action.is_none());
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
    assert_eq!(detail.input_bar_text_for_test(), "previous prompt");
}

#[test]
fn reset_to_root_resets_focused_panel() {
    let mut detail = mk_view();
    press(&mut detail, KeyCode::Tab);
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::Workers);

    detail.reset_to_root();

    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
}
