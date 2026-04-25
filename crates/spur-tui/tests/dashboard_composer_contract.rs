//! Regression tests for DashboardView composer key-ownership contract.
//!
//! These verify that the dashboard routes keys based on pre-key state
//! (empty vs non-empty input bar) rather than post-edit rescue logic.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_core::ExecutorLineage;
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
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
fn non_empty_multiline_up_reaches_input_bar() {
    let mut dashboard = DashboardView::new();
    // Seed a two-line draft with cursor at the end via paste (enters Compose mode).
    dashboard.handle_paste("line1\nline2");

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
