use spur_core::InteractiveInput;
use spur_interactive::{validate_frontend_command, ReviewSubmission};

#[tokio::test]
async fn shutdown_completes_promptly_even_with_outstanding_continuation_sender() {
    struct DropAck(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropAck {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel();
    let orch_handle = tokio::spawn(async move {
        let _drop_ack = DropAck(Some(drop_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    started_rx.await.expect("stuck orchestrator task started");
    let host = spur_interactive::InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        orch_handle,
    );
    let _outstanding_handle = host.handle();

    let started = std::time::Instant::now();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), host.shutdown())
        .await
        .expect("shutdown entered a second timeout window")
        .expect_err("stuck orchestrator shutdown should report the emergency abort");

    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert!(error.to_string().contains("timed out after 2s"));
    tokio::time::timeout(std::time::Duration::from_millis(250), drop_rx)
        .await
        .expect("shutdown returned before aborted orchestrator acknowledged Drop")
        .expect("orchestrator Drop acknowledgement sender disappeared");
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
