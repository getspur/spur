//! Pure-function key→decision mapping. No ratatui needed.

use spur_core::ReviewDecision;
use spur_tui::components::review_card::decision_for_key;

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
    use spur_tui::UserInput;
    use spur_core::ReviewDecision;
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
