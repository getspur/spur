//! Integration test: end-to-end wiring from keystrokes on `SessionDetailView`
//! through the completion popup and `SubmitRouter` to the emitted `Action`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn ctrl(v: &mut SessionDetailView, c: char) -> Option<Action> {
    v.handle_key(
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        &test_ctx(),
    )
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

#[test]
fn plain_text_submit_produces_text_block() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
    );
    type_str(&mut v, "hello");
    let act = press(&mut v, KeyCode::Enter).expect("action");
    match act {
        Action::SendMessage {
            blocks, interrupt, ..
        } => {
            assert!(!interrupt);
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                spur_acp::ContentBlock::Text(t) => assert_eq!(t.text, "hello"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn slash_help_fires_show_help_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
    );
    type_str(&mut v, "/");
    // popup is open; Enter accepts the first row (which is /help from spur-local)
    let _ = press(&mut v, KeyCode::Enter); // accept → inserts "/help " into InputBar
    let act = press(&mut v, KeyCode::Enter); // second Enter → submit
    assert!(matches!(act, Some(Action::ShowHelp)));
}

#[test]
fn ctrl_r_history_restore_preserves_resource_links() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
    );
    v.seed_input_history(vec![InputHistoryEntry::new(InputStateSnapshot::new(
        "check @src/foo.rs".into(),
        vec![ProtectedRange {
            start: 6,
            end: 17,
            uri: "file:///abs/src/foo.rs".into(),
            name: "src/foo.rs".into(),
        }],
    ))]);

    let _ = ctrl(&mut v, 'r');
    let _ = press(&mut v, KeyCode::Enter); // accept first history hit
    let act = press(&mut v, KeyCode::Enter).expect("send action");

    match act {
        Action::SendMessage { blocks, .. } => {
            assert_eq!(blocks.len(), 2);
            match (&blocks[0], &blocks[1]) {
                (spur_acp::ContentBlock::Text(t), spur_acp::ContentBlock::ResourceLink(r)) => {
                    assert_eq!(t.text, "check ");
                    assert_eq!(r.name, "src/foo.rs");
                    assert_eq!(r.uri, "file:///abs/src/foo.rs");
                }
                other => panic!("expected [Text, ResourceLink], got {:?}", other),
            }
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}
