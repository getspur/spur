//! Pure-function key→decision mapping. No ratatui needed.

use spur_core::ReviewDecision;
use spur_tui::components::review_card::decision_for_key;

fn test_ctx_with_lineage(lineage: &spur_core::lineage::projection::ExecutorLineage) -> spur_tui::views::ViewContext<'_> {
    spur_tui::test_support::test_view_ctx(lineage)
}

#[test]
fn approve_key_maps_to_approve_decision() {
    let d = decision_for_key('a', None);
    assert!(matches!(d, Some(ReviewDecision::Approve)));
}

#[test]
fn deny_key_with_reason_maps_to_reject() {
    let d = decision_for_key('d', Some("bad".into()));
    match d {
        Some(ReviewDecision::Reject { reason }) => assert_eq!(reason, "bad"),
        other => panic!("expected Reject, got {:?}", other),
    }
}

#[test]
fn modify_key_maps_to_modify() {
    let d = decision_for_key('m', Some("add tests".into()));
    assert!(matches!(d, Some(ReviewDecision::Modify { .. })));
}

#[test]
fn retry_key_maps_to_retry() {
    let d = decision_for_key('R', None);
    assert!(matches!(d, Some(ReviewDecision::Retry { .. })));
}

#[test]
fn unknown_key_returns_none() {
    let d = decision_for_key('z', None);
    assert!(d.is_none());
}

#[test]
fn submit_review_carries_attempt_n() {
    use spur_core::ReviewDecision;
    use spur_tui::UserInput;
    let input = UserInput::SubmitReview {
        executor_id: "exec-1".into(),
        attempt_n: 2,
        decision: ReviewDecision::Approve,
    };
    match input {
        UserInput::SubmitReview { attempt_n, .. } => assert_eq!(attempt_n, 2),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn dashboard_reads_attempt_n_from_lineage_on_submit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ExecutorLineage};
    use spur_tui::action::Action;
    use spur_tui::components::detail_pane::DetailTab;
    use spur_tui::views::dashboard::DashboardView;
    use spur_tui::views::View;

    // Build a lineage where the focused node has pending_review.attempt_n = 3.
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
        attempt_n: 3,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
        },
    }));
    assert_eq!(
        lineage
            .node(&ExecutorId::new("e1"))
            .unwrap()
            .pending_review
            .as_ref()
            .unwrap()
            .attempt_n,
        3
    );

    // Construct dashboard view; focus the node and switch to the Review tab
    // so that the 'a' keypress is interpreted as an approve review decision.
    let mut dashboard = DashboardView::new();
    dashboard.set_focused_node(Some(ExecutorId::new("e1")));
    dashboard.detail_pane_mut().current_tab = DetailTab::Review;

    // Simulate pressing 'a' — InputBar appends the char, then the review-key
    // intercept emits Action::SubmitReview. The attempt_n MUST be read from
    // the lineage's pending_review (3), not the unwrap_or(1) fallback.
    let action = dashboard.handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &test_ctx_with_lineage(&lineage),
    );

    match action {
        Some(Action::SubmitReview {
            executor_id,
            attempt_n,
            ..
        }) => {
            assert_eq!(executor_id, "e1");
            assert_eq!(
                attempt_n, 3,
                "attempt_n must be read from lineage, not defaulted"
            );
        }
        other => panic!(
            "expected Action::SubmitReview with attempt_n=3, got {:?}",
            other
        ),
    }
}
