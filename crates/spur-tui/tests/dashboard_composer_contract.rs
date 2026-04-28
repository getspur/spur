//! Regression tests for DashboardView composer key-ownership contract.
//!
//! These verify that the dashboard routes keys based on pre-key state
//! (empty vs non-empty input bar) rather than post-edit rescue logic.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_core::ExecutorLineage;
use spur_tui::action::Action;
use spur_tui::mentions::WorkerMentionDescriptor;
use spur_tui::views::dashboard::DashboardMode;
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn press(dashboard: &mut DashboardView, code: KeyCode) -> Option<Action> {
    dashboard.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn type_str(dashboard: &mut DashboardView, s: &str) {
    for c in s.chars() {
        let _ = press(dashboard, KeyCode::Char(c));
    }
}

#[test]
fn empty_dashboard_j_routes_to_view_action() {
    let mut dashboard = DashboardView::new();
    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(spur_tui::action::Action::ScrollDown)),
        "empty input bar: 'j' must be a view action, got {:?}",
        action
    );
    // InputBar must not have been modified.
    assert_eq!(dashboard.input_bar_text_for_test(), "");
}

#[test]
fn non_empty_dashboard_j_stays_in_input_bar() {
    let mut dashboard = DashboardView::new();
    dashboard.handle_paste("hello");

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        action.is_none(),
        "non-empty input bar: 'j' must stay in composer, got {:?}",
        action
    );
    assert_eq!(dashboard.input_bar_text_for_test(), "helloj");
}

#[test]
fn dashboard_pre_session_mention_accept_submits_resource_link() {
    let mut dashboard = DashboardView::new();

    type_str(&mut dashboard, "please summarize @Cargo.toml");
    let _ = press(&mut dashboard, KeyCode::Tab);
    let action = press(&mut dashboard, KeyCode::Enter).expect("submit action");

    match action {
        Action::NewSessionWithMessage { blocks, .. } => {
            assert!(
                blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ResourceLink(_))),
                "expected ResourceLink in pre-session Dashboard blocks, got {blocks:?}"
            );
        }
        other => panic!("expected NewSessionWithMessage, got {other:?}"),
    }
}

#[test]
fn dashboard_command_registry_includes_spur_local_commands() {
    let dashboard = DashboardView::new();
    let names: Vec<_> = dashboard
        .command_registry_for_test()
        .list()
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    assert!(names.contains(&"help".to_string()), "names={names:?}");
    assert!(names.contains(&"quit".to_string()), "names={names:?}");
}

#[test]
fn dashboard_pre_session_slash_help_dispatches_locally() {
    let mut dashboard = DashboardView::new();

    type_str(&mut dashboard, "/help");
    let _ = press(&mut dashboard, KeyCode::Esc);
    let action = press(&mut dashboard, KeyCode::Enter);

    assert!(
        matches!(action, Some(Action::ShowHelp)),
        "expected local ShowHelp action, got {action:?}"
    );
}

#[test]
fn dashboard_pre_session_slash_clear_dispatches_locally() {
    let mut dashboard = DashboardView::new();

    type_str(&mut dashboard, "/clear");
    let _ = press(&mut dashboard, KeyCode::Esc);
    let action = press(&mut dashboard, KeyCode::Enter);

    assert!(
        !matches!(
            &action,
            Some(Action::NewSessionWithMessage { blocks, .. })
                if blocks.iter().any(|block| matches!(
                    block,
                    ContentBlock::Text(text) if text.text.contains("/clear")
                ))
        ),
        "raw /clear must not be submitted as NewSessionWithMessage, got {action:?}"
    );
    if let Some(Action::NewSessionWithMessage { blocks, .. }) = &action {
        assert!(
            blocks.is_empty(),
            "pre-session /clear must not submit content blocks, got {blocks:?}"
        );
    }
    assert!(
        matches!(action, Some(Action::ClearSession)),
        "expected local ClearSession action, got {action:?}"
    );
}

#[test]
fn dashboard_pre_session_worker_mention_prepends_hint_and_resource_link() {
    let mut dashboard = DashboardView::new();
    dashboard.set_worker_snapshot(vec![WorkerMentionDescriptor {
        name: "codex".into(),
        description: Some("Writes Rust".into()),
        tier: Some("generalist".into()),
    }]);

    type_str(&mut dashboard, "@worker:codex");
    let _ = press(&mut dashboard, KeyCode::Tab);
    type_str(&mut dashboard, " hello");
    let action = press(&mut dashboard, KeyCode::Enter).expect("submit action");

    let blocks = match action {
        Action::NewSessionWithMessage { blocks, .. } => blocks,
        other => panic!("expected NewSessionWithMessage, got {other:?}"),
    };

    assert!(
        matches!(&blocks[0], ContentBlock::Text(t)
            if t.text.starts_with("[UI hint]") && t.text.contains("codex")),
        "expected [UI hint] Text at blocks[0], got {:?}",
        blocks[0]
    );
    assert!(
        blocks.iter().skip(1).any(|block| matches!(
            block,
            ContentBlock::ResourceLink(link) if link.uri == "worker://codex"
        )),
        "expected worker ResourceLink after hint, got {blocks:?}"
    );
}

#[test]
fn dashboard_esc_closes_active_completion_before_exiting_compose() {
    let mut dashboard = DashboardView::new();

    type_str(&mut dashboard, "@");
    assert_eq!(dashboard.mode(), DashboardMode::Compose);

    let action = press(&mut dashboard, KeyCode::Esc);

    assert!(
        action.is_none(),
        "Esc should close completion, got {action:?}"
    );
    assert_eq!(dashboard.mode(), DashboardMode::Compose);
    assert_eq!(dashboard.input_bar_text_for_test(), "@");
}

#[test]
fn non_empty_multiline_up_reaches_input_bar() {
    let mut dashboard = DashboardView::new();
    // Enter Compose mode, then seed a two-line draft with cursor at the end.
    // Multiline paste is atomized, while this test is about multiline cursor routing.
    dashboard.handle_paste("line1");
    dashboard
        .input_bar_mut_for_test()
        .set_text("line1\nline2".to_string(), "line1\nline2".len());

    let before = dashboard.input_bar_mut_for_test().cursor();
    let action = dashboard.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    let after = dashboard.input_bar_mut_for_test().cursor();

    assert!(
        action.is_none(),
        "non-empty multiline: Up must be consumed by InputBar, got {:?}",
        action
    );
    assert!(
        after < before,
        "Up must move cursor up in multiline draft: before={} after={}",
        before,
        after
    );
}

#[test]
fn empty_review_tab_decision_routes_pre_key() {
    use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ReviewDecision};
    use spur_tui::action::Action;
    use spur_tui::components::detail_pane::DetailTab;

    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "e1".into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "t".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "e1".into(),
        attempt_n: 2,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));

    let mut dashboard = DashboardView::new();
    dashboard.set_focused_node(Some(ExecutorId::new("e1")));
    dashboard.detail_pane_mut().jump_to_tab(DetailTab::Review);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
        &spur_tui::test_support::test_view_ctx(&lineage),
    );

    match action {
        Some(Action::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        }) => {
            assert_eq!(executor_id, "e1");
            assert_eq!(attempt_n, 2);
            assert!(matches!(decision, ReviewDecision::Approve));
        }
        other => panic!(
            "expected Action::SubmitReview with attempt_n=2, got {:?}",
            other
        ),
    }

    // InputBar must remain untouched.
    assert_eq!(dashboard.input_bar_text_for_test(), "");
}

#[test]
fn non_empty_tab_stays_in_composer() {
    let mut dashboard = DashboardView::new();
    dashboard.handle_paste("hello");
    let before = dashboard.input_bar_text_for_test();

    let action = dashboard.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &test_ctx());

    assert!(
        action.is_none(),
        "compose mode: Tab must stay in composer, got {:?}",
        action
    );
    // In Compose mode Tab is consumed by the composer (tui-textarea may expand
    // it to spaces, so we just verify the text changed).
    let after = dashboard.input_bar_text_for_test();
    assert_ne!(
        after, before,
        "compose mode: Tab must mutate composer text, got same: {}",
        after
    );
}

#[test]
fn non_empty_esc_emacs_does_not_unfocus_or_navigate_back() {
    let mut dashboard = DashboardView::new();
    // Default mode is Emacs; Esc is not consumed by the composer.
    dashboard.handle_paste("hello");
    let before = dashboard.input_bar_text_for_test();

    let action = dashboard.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());

    assert!(
        action.is_none(),
        "non-empty Emacs: Esc must be a no-op, got {:?}",
        action
    );
    assert_eq!(
        dashboard.input_bar_text_for_test(),
        before,
        "non-empty Emacs: Esc must not mutate composer text"
    );
}

#[test]
fn non_empty_esc_vim_normal_does_not_unfocus_or_navigate_back() {
    use spur_tui::components::input_bar::{EditMode, VimMode};

    let mut dashboard = DashboardView::new();
    dashboard.set_edit_mode(EditMode::Vim(VimMode::Normal));
    dashboard.handle_paste("hello");
    let before = dashboard.input_bar_text_for_test();

    let action = dashboard.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());

    assert!(
        action.is_none(),
        "non-empty Vim Normal: Esc must be a no-op, got {:?}",
        action
    );
    assert_eq!(
        dashboard.input_bar_text_for_test(),
        before,
        "non-empty Vim Normal: Esc must not mutate composer text"
    );
}

#[test]
fn vim_normal_review_tab_decision_routes_to_view() {
    use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ReviewDecision};
    use spur_tui::action::Action;
    use spur_tui::components::detail_pane::DetailTab;
    use spur_tui::components::input_bar::{EditMode, VimMode};

    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "e1".into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "t".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "e1".into(),
        attempt_n: 2,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));

    let mut dashboard = DashboardView::new();
    dashboard.set_edit_mode(EditMode::Vim(VimMode::Normal));
    dashboard.set_focused_node(Some(ExecutorId::new("e1")));
    dashboard.detail_pane_mut().jump_to_tab(DetailTab::Review);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
        &spur_tui::test_support::test_view_ctx(&lineage),
    );

    match action {
        Some(Action::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        }) => {
            assert_eq!(executor_id, "e1");
            assert_eq!(attempt_n, 2);
            assert!(matches!(decision, ReviewDecision::Approve));
        }
        other => panic!(
            "expected Action::SubmitReview with attempt_n=2, got {:?}",
            other
        ),
    }

    // InputBar must remain untouched — the 'A' key was routed to the review
    // card, not into Vim insert mode.
    assert_eq!(dashboard.input_bar_text_for_test(), "");
}

#[test]
fn vim_normal_focused_node_o_toggles_observe_not_compose() {
    // Regression: vim-Normal `o` on a focused detail-pane node must hit the
    // observe-toggle binding, not be swallowed as vim's "open line below"
    // into the input bar. The pre-fix routing consulted the vim
    // compose-entry whitelist before `is_view_action_char`, making the
    // observe binding unreachable in vim mode.
    use spur_acp::{
        ContentBlock, ContentChunk, Role, SessionId, SessionUpdate, SpurEvent, SpurEventBody,
        TextContent,
    };
    use spur_core::{ExecutorId, ExecutorLineage};
    use spur_tui::components::input_bar::{EditMode, VimMode};
    use spur_tui::worker_streams::WorkerStreams;

    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "e1".into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: "claude".into(),
        role: Role::Executor,
        task_spec: "t".into(),
    }));

    let mut ws = WorkerStreams::new();
    ws.route(
        "e1",
        "claude",
        &SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("seed"),
        ))),
    );
    let collapsed_before = ws.get("e1").expect("trace seeded").observe_collapsed();

    let mut dashboard = DashboardView::new();
    dashboard.set_edit_mode(EditMode::Vim(VimMode::Normal));
    dashboard.set_focused_node(Some(ExecutorId::new("e1")));
    // DetailPane defaults to DetailTab::Stream; no need to set explicitly.

    let action = dashboard.handle_key_with_worker_streams(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
        &lineage,
        &mut ws,
    );

    assert!(
        action.is_none(),
        "vim-Normal `o` on focused node must not emit an action, got {:?}",
        action
    );
    assert_eq!(
        dashboard.input_bar_text_for_test(),
        "",
        "vim-Normal `o` on focused node must NOT route into the input bar"
    );
    assert_eq!(
        dashboard.mode(),
        DashboardMode::Navigate,
        "vim-Normal `o` on focused node must NOT flip dashboard into Compose"
    );
    let collapsed_after = ws.get("e1").expect("trace").observe_collapsed();
    assert_ne!(
        collapsed_before, collapsed_after,
        "vim-Normal `o` on focused node must toggle observe-collapsed on the trace"
    );
}

// ── Page-wise navigation contract (PR1: counted-selection actions) ─────────
//
// These tests lock in the post-PR1 contract that PageUp/PageDown in the
// Agents panel emit exactly one counted action — not five inline mutations
// followed by a misleading ScrollUp/ScrollDown. Regressing this would
// re-introduce the latent coupling closed by commit 554e98b5.

#[test]
fn empty_dashboard_pageup_in_agents_emits_select_prev_by_5() {
    use spur_tui::views::dashboard::Panel;

    let mut dashboard = DashboardView::new();
    dashboard.set_focused_panel(Panel::Agents);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectPrevBy(5))),
        "PageUp in Agents panel must emit SelectPrevBy(5) exactly once, got {:?}",
        action
    );
}

#[test]
fn empty_dashboard_pagedown_in_agents_emits_select_next_by_5() {
    use spur_tui::views::dashboard::Panel;

    let mut dashboard = DashboardView::new();
    dashboard.set_focused_panel(Panel::Agents);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectNextBy(5))),
        "PageDown in Agents panel must emit SelectNextBy(5) exactly once, got {:?}",
        action
    );
}

#[test]
fn empty_dashboard_up_in_agents_emits_select_prev_by_1() {
    use spur_tui::views::dashboard::Panel;

    let mut dashboard = DashboardView::new();
    dashboard.set_focused_panel(Panel::Agents);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectPrevBy(1))),
        "Up in Agents panel must emit SelectPrevBy(1) exactly once, got {:?}",
        action
    );
}

#[test]
fn empty_dashboard_down_in_agents_emits_select_next_by_1() {
    use spur_tui::views::dashboard::Panel;

    let mut dashboard = DashboardView::new();
    dashboard.set_focused_panel(Panel::Agents);

    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::SelectNextBy(1))),
        "Down in Agents panel must emit SelectNextBy(1) exactly once, got {:?}",
        action
    );
}
