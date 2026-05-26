//! Integration: @mention and /slash popups route through PickerShell but
//! preserve pre-Phase-3 user-visible behavior:
//!   * Typing `/he` + Enter + Enter dispatches ShowHelp (today's test:
//!     slash_help_fires_show_help_action).
//!   * Typing `@` opens a PickerShell with MentionQuerySource.
//!   * Tab with a selected mention row inserts a ResourceLink via
//!     insert_atom and drops the `@query` prefix.
//!   * Esc closes the shell without mutating the InputBar trigger prefix
//!     (the typed `@foo` stays as literal text).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::app::App;
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
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

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

fn ctrl_press(v: &mut SessionDetailView, c: char) -> Option<Action> {
    v.handle_key(
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        &test_ctx(),
    )
}

fn app_press(app: &mut App, code: KeyCode) {
    app.handle_crossterm_event_for_test(KeyEvent::new(code, KeyModifiers::NONE));
}

fn app_ctrl_press(app: &mut App, c: char) {
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

#[test]
fn slash_help_via_picker_shell_dispatches_show_help() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    // Type '/' → opens slash PickerShell, first row = /help (spur-local).
    type_str(&mut v, "/");
    // Enter → accept selected row, replaces '/' with '/help '
    let _ = press(&mut v, KeyCode::Enter);
    // Second Enter → submit '/help ' → ShowHelp action.
    let act = press(&mut v, KeyCode::Enter);
    assert!(
        matches!(act, Some(Action::ShowHelp)),
        "expected Some(Action::ShowHelp), got {:?}",
        act
    );
}

#[test]
fn mention_tab_inserts_resource_link_on_submit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    // Type "@NOT" — opens mention PickerShell; source queries registry.
    type_str(&mut v, "@NOT");
    // Tab → accept mention row; prefix '@NOT' cleared, protected atom inserted.
    let _ = press(&mut v, KeyCode::Tab);
    // Enter → submit; outbound blocks should include a ResourceLink.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            assert!(
                blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ResourceLink(_))),
                "expected a ResourceLink in outbound blocks, got {:?}",
                blocks
            );
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn mention_esc_leaves_typed_at_query_literal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    type_str(&mut v, "@NOT");
    let _ = press(&mut v, KeyCode::Esc);
    // After Esc, typed '@NOT' stays; submit carries it as plain text.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            // No ResourceLink (never accepted); '@NOT' is in the text block.
            assert!(
                !blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ResourceLink(_))),
                "did not expect a ResourceLink after Esc, got {:?}",
                blocks
            );
            let text_concat: String = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            assert!(
                text_concat.contains("@NOT"),
                "expected '@NOT' literal in outbound text, got {:?}",
                text_concat
            );
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn typing_space_after_at_closes_mention_shell() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    type_str(&mut v, "@NOT");
    // Space terminates the mention trigger; detect() returns None; shell closes.
    press(&mut v, KeyCode::Char(' '));
    // Subsequent Enter submits the literal text including '@NOT '.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            let text_concat: String = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            assert!(text_concat.contains("@NOT "), "got {:?}", text_concat);
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn trigger_picker_blocks_ctrl_p_ctrl_n_from_reaching_input_bar_history() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    // Seed input history so history_prev() would mutate the bar if reached.
    v.seed_input_history(vec![spur_tui::input_history::InputHistoryEntry::from_text(
        "previous history entry",
    )]);

    // Open a trigger-driven mention picker.
    type_str(&mut v, "@NOT");
    assert_eq!(v.input_bar_text(), "@NOT", "draft before Ctrl+P");

    // Ctrl+P must NOT reach InputBar history navigation.
    let _ = ctrl_press(&mut v, 'p');
    assert_eq!(
        v.input_bar_text(),
        "@NOT",
        "Ctrl+P must not mutate the hidden composer while a trigger-driven picker is open"
    );

    // Also verify Ctrl+N is blocked.
    let _ = ctrl_press(&mut v, 'n');
    assert_eq!(
        v.input_bar_text(),
        "@NOT",
        "Ctrl+N must not mutate the hidden composer while a trigger-driven picker is open"
    );

    // Dismiss the picker so we can submit the untouched trigger text.
    let _ = press(&mut v, KeyCode::Esc);

    // Submit should still carry the original trigger text, not history.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            let text_concat: String = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            assert!(
                text_concat.contains("@NOT"),
                "expected '@NOT' literal in outbound text, got {:?}",
                text_concat
            );
            assert!(
                !text_concat.contains("previous history entry"),
                "history must not leak into submission, got {:?}",
                text_concat
            );
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn dashboard_ctrl_p_with_open_picker_moves_picker_not_history() {
    let mut app = App::new_for_tests();
    app.dashboard_mut_for_test().seed_input_history(vec![
        spur_tui::input_history::InputHistoryEntry::from_text("previous history entry"),
    ]);
    app.open_dashboard_slash_picker_for_test();

    assert!(app.dashboard_for_test().completion_active_for_test());
    assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "/");

    app_ctrl_press(&mut app, 'p');

    assert_eq!(
        app.dashboard_for_test().input_bar_text_for_test(),
        "/",
        "Ctrl+P must move the picker, not recall input history"
    );
    assert!(app.dashboard_for_test().completion_active_for_test());

    app_press(&mut app, KeyCode::Enter);
    let accepted = app.dashboard_for_test().input_bar_text_for_test();
    assert!(
        accepted.starts_with('/'),
        "picker accept should insert a slash command, got {:?}",
        accepted
    );
    assert_ne!(
        accepted, "/help ",
        "Ctrl+P should move off the initial /help row before accept"
    );
    assert_ne!(accepted, "previous history entry");
}

#[test]
fn dashboard_ctrl_n_with_open_picker_moves_picker_not_history() {
    let mut app = App::new_for_tests();
    app.dashboard_mut_for_test().seed_input_history(vec![
        spur_tui::input_history::InputHistoryEntry::from_text("previous history entry"),
    ]);
    app.open_dashboard_slash_picker_for_test();

    assert!(app.dashboard_for_test().completion_active_for_test());
    assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "/");

    app_ctrl_press(&mut app, 'n');

    assert_eq!(
        app.dashboard_for_test().input_bar_text_for_test(),
        "/",
        "Ctrl+N must move the picker, not recall input history"
    );
    assert!(app.dashboard_for_test().completion_active_for_test());

    app_press(&mut app, KeyCode::Enter);
    let accepted = app.dashboard_for_test().input_bar_text_for_test();
    assert!(
        accepted.starts_with('/'),
        "picker accept should insert a slash command, got {:?}",
        accepted
    );
    assert_ne!(
        accepted, "/help ",
        "Ctrl+N should move off the initial /help row before accept"
    );
    assert_ne!(accepted, "previous history entry");
}
