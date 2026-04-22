use spur_core::InteractiveInput;
use spur_interactive::{validate_frontend_command, ReviewSubmission};

#[test]
fn reject_submit_review_on_command_lane() {
    let err = validate_frontend_command(&InteractiveInput::SubmitReview {
        executor_id: "exec-42".into(),
        attempt_n: 2,
        decision: spur_acp::ReviewDecision::Approve,
    })
    .unwrap_err();

    assert!(err.to_string().contains("send_review"));
}

#[test]
fn review_submission_converts_to_submit_review() {
    let input = ReviewSubmission {
        executor_id: "exec-42".into(),
        attempt_n: 2,
        decision: spur_acp::ReviewDecision::Retry {
            new_constraints: String::new(),
        },
    }
    .into_input();

    assert!(matches!(
        input,
        InteractiveInput::SubmitReview {
            executor_id,
            attempt_n: 2,
            decision: spur_acp::ReviewDecision::Retry { new_constraints },
        } if executor_id == "exec-42" && new_constraints.is_empty()
    ));
}
