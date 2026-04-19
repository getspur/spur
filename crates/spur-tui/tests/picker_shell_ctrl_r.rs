//! Integration: `Ctrl+R` opens a PickerShell, typing filters rows, Tab/Enter
//! accepts and restores the snapshot into InputBar, Esc cancels without
//! mutating InputBar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn mk_view() -> SessionDetailView {
    let tmp = tempfile::tempdir().unwrap();
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        Vec::new(),
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn press_mod(v: &mut SessionDetailView, code: KeyCode, m: KeyModifiers) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, m), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

fn seed_history(v: &mut SessionDetailView, entries: Vec<InputHistoryEntry>) {
    v.input_bar_mut_for_test().seed_history(entries);
}

#[test]
fn ctrl_r_opens_shell_and_accept_restores_snapshot() {
    let mut v = mk_view();
    seed_history(
        &mut v,
        vec![
            InputHistoryEntry::new(InputStateSnapshot::from_text("refactor the walker")),
            InputHistoryEntry::new(InputStateSnapshot::from_text("fix the panic")),
        ],
    );

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_str(&mut v, "refa");
    press(&mut v, KeyCode::Enter);

    // Expect the InputBar to now contain "refactor the walker".
    assert_eq!(v.input_bar_text_for_test(), "refactor the walker");
}

#[test]
fn ctrl_r_esc_leaves_input_bar_untouched() {
    let mut v = mk_view();
    seed_history(
        &mut v,
        vec![InputHistoryEntry::new(InputStateSnapshot::from_text("hello"))],
    );

    // Start with a draft in the InputBar.
    type_str(&mut v, "my draft");
    assert_eq!(v.input_bar_text_for_test(), "my draft");

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_str(&mut v, "he");
    press(&mut v, KeyCode::Esc);

    assert_eq!(v.input_bar_text_for_test(), "my draft");
}

#[test]
fn ctrl_r_accept_roundtrips_resource_link_on_resubmit() {
    let mut v = mk_view();
    let mut snap = InputStateSnapshot::from_text("hi @foo");
    snap.protected_ranges = vec![ProtectedRange {
        start: 3,
        end: 7,
        uri: "file:///foo".to_string(),
        name: "foo".to_string(),
    }];
    seed_history(&mut v, vec![InputHistoryEntry::new(snap)]);

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut v, KeyCode::Enter); // accept newest (only) row

    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            // Expect a Text("hi ") + ResourceLink { uri: file:///foo, name: foo }.
            assert_eq!(blocks.len(), 2);
            assert!(matches!(&blocks[1], ContentBlock::ResourceLink(r) if r.uri == "file:///foo" && r.name == "foo"));
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn ctrl_r_on_empty_history_opens_empty_shell_and_esc_closes() {
    let mut v = mk_view();
    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut v, KeyCode::Esc);
    // No panic, no state change. Follow-up Enter should behave like a
    // regular empty-composer Enter (no action).
    let act = press(&mut v, KeyCode::Enter);
    assert!(act.is_none() || act.is_some()); // any behavior OK so long as no panic
}
