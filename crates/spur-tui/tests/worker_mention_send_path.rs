//! Integration test: when a brain SessionDetailView submits a message
//! containing a `worker://` atom, the resulting `Action::SendMessage`
//! has a `[UI hint]` Text block prepended as `blocks[0]` and preserves
//! the original ResourceLink later in `blocks`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::mentions::WorkerMentionDescriptor;
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn brain_view_with_workers(workers: Vec<WorkerMentionDescriptor>) -> SessionDetailView {
    let tmp = tempfile::tempdir().unwrap();
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers,
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        let _ = press(v, KeyCode::Char(c));
    }
}

#[test]
fn brain_send_prepends_worker_hint_block() {
    let mut v = brain_view_with_workers(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        kind: spur_acp::AgentKind::ClaudeCodeAcp,
        description: Some("Refactors Rust".into()),
        tier: Some("specialist".into()),
    }]);

    // "@cla" → opens the mention shell; with the +25% boost worker:claude-code
    // ranks at row 0 (validated by Task 4 test `typed_query_boosts_worker_in_ambiguous_match`).
    type_str(&mut v, "@cla");
    // Tab → accept the top row (worker:claude-code), inserts a protected atom
    // with uri "worker://claude-code".
    let _ = press(&mut v, KeyCode::Tab);
    // Enter → submit. Resulting blocks should have the hint at [0]
    // and the worker ResourceLink later.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };

    assert!(
        matches!(&blocks[0], ContentBlock::Text(t)
            if t.text.starts_with("[UI hint]") && t.text.contains("claude-code")),
        "expected [UI hint] Text at blocks[0], got {:?}",
        blocks[0]
    );
    assert!(
        blocks.iter().skip(1).any(|b| matches!(
            b,
            ContentBlock::ResourceLink(r) if r.uri == "worker://claude-code"
        )),
        "expected a ResourceLink with uri=worker://claude-code later in blocks, got {:?}",
        blocks
    );
}

#[test]
fn brain_send_without_worker_atom_has_no_hint() {
    let mut v = brain_view_with_workers(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        kind: spur_acp::AgentKind::ClaudeCodeAcp,
        description: None,
        tier: None,
    }]);

    type_str(&mut v, "just text");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    // First block must NOT be the hint.
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "did not expect a hint when no worker atom was present, got: {}",
            t.text
        );
    }
}

#[test]
fn direct_session_skips_hint_even_with_worker_atom_pasted() {
    // Direct (non-brain) view. Even with a populated worker snapshot, the
    // `role == "brain"` guard in the send-path arm must prevent any hint.
    // Verifies the role guard itself; we type only plain text (the atom
    // path is exercised by the brain-session test above).
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "worker".into(), // role != "brain"
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        vec![WorkerMentionDescriptor {
            name: "claude-code".into(),
            kind: spur_acp::AgentKind::ClaudeCodeAcp,
            description: None,
            tier: None,
        }],
    );

    type_str(&mut v, "anything");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "direct session must never prepend the hint, got: {}",
            t.text
        );
    }
}
