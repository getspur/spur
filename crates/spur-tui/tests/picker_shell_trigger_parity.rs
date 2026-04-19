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
                blocks.iter().any(|b| matches!(b, ContentBlock::ResourceLink(_))),
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
                !blocks.iter().any(|b| matches!(b, ContentBlock::ResourceLink(_))),
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
