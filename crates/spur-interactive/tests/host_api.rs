use spur_core::InteractiveInput;
use spur_interactive::{validate_frontend_command, ReviewSubmission};

#[tokio::test]
async fn shutdown_completes_promptly_even_with_outstanding_continuation_sender() {
    let repo_root =
        std::env::temp_dir().join(format!("spur-interactive-host-api-{}", std::process::id()));
    std::fs::create_dir_all(&repo_root).unwrap();
    let orch =
        spur_core::Orchestrator::new(repo_root, spur_acp::config::SpurConfig::default(), None)
            .unwrap();
    let host = spur_interactive::InteractiveFrontendHost::spawn(orch, None);
    let _outstanding_handle = host.handle();

    tokio::time::timeout(std::time::Duration::from_secs(2), host.shutdown())
        .await
        .expect("shutdown should complete within 2 seconds")
        .unwrap();
}

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

#[tokio::test]
async fn send_review_uses_the_review_lane() {
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, mut review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    let host = spur_interactive::InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();

    handle
        .send_review(ReviewSubmission {
            executor_id: "exec-7".into(),
            attempt_n: 3,
            decision: spur_acp::ReviewDecision::Retry {
                new_constraints: String::new(),
            },
        })
        .await
        .unwrap();

    assert!(user_rx.try_recv().is_err());
    let input = review_rx.recv().await.unwrap();
    assert!(matches!(
        input,
        InteractiveInput::SubmitReview {
            executor_id,
            attempt_n: 3,
            decision: spur_acp::ReviewDecision::Retry { new_constraints },
        } if executor_id == "exec-7" && new_constraints.is_empty()
    ));
}

#[tokio::test]
async fn host_streams_can_only_be_taken_once() {
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut host = spur_interactive::InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );

    assert!(host.take_event_stream().is_some());
    assert!(host.take_event_stream().is_none());
    assert!(host.take_permission_stream().is_some());
    assert!(host.take_permission_stream().is_none());
}
