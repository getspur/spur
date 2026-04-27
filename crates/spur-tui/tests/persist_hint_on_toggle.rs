//! Unit coverage for `SessionDetailView::push_persist_hint`.
//!
//! The runtime integration (Action::ToggleVimMode → push_persist_hint via
//! divergence check) is exercised by manual smoke in T6; here we just
//! assert the hint text shape is exactly what the T4 CLI accepts as a
//! positional invocation.

use spur_acp::SessionId;
use spur_tui::views::session_detail::SessionDetailView;

fn make_view() -> SessionDetailView {
    let tmp = tempfile::tempdir().unwrap();
    SessionDetailView::new(
        SessionId::new(),
        "codex".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("codex"),
        Vec::new(),
    )
}

#[test]
fn push_persist_hint_vim_emits_lowercase_cli_invocation() {
    let mut view = make_view();
    view.push_persist_hint("Vim");
    let last = view
        .react_trace()
        .entries()
        .last()
        .expect("hint must push a trace entry");
    assert!(
        last.text.contains("spur config set tui.edit_mode vim"),
        "hint must embed exact T4 CLI form; got: {}",
        last.text
    );
    assert!(
        last.text.starts_with("Vim mode (session)."),
        "hint must lead with the runtime mode label; got: {}",
        last.text
    );
}

#[test]
fn push_persist_hint_emacs_emits_lowercase_cli_invocation() {
    let mut view = make_view();
    view.push_persist_hint("Emacs");
    let last = view.react_trace().entries().last().unwrap();
    assert!(
        last.text.contains("spur config set tui.edit_mode emacs"),
        "got: {}",
        last.text
    );
}
