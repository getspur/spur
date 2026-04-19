use spur_acp::ReviewDecision;
use spur_core::{ExecutorId, ReviewSink};

#[tokio::test]
async fn register_then_submit_delivers_decision() {
    let sink = ReviewSink::new();
    let handle = sink
        .register_handle(ExecutorId("e1".into()), 1)
        .await
        .expect("registered");
    let rx = handle.into_rx();
    let submitted = sink
        .submit(ExecutorId("e1".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(submitted, "submit should succeed");
    let decision = rx.await.expect("decision");
    assert!(matches!(decision, ReviewDecision::Approve));
}

#[tokio::test]
async fn attempt_n_mismatch_drops_decision() {
    let sink = ReviewSink::new();
    let handle = sink
        .register_handle(ExecutorId("e1".into()), 2)
        .await
        .expect("registered");
    let rx = handle.into_rx();
    let submitted = sink
        .submit(
            ExecutorId("e1".into()),
            1, // stale attempt
            ReviewDecision::Reject { reason: "r".into() },
        )
        .await;
    assert!(!submitted, "stale attempt_n must be dropped");
    // Sender still in place — legitimate attempt-2 reviewer can still submit.
    let submitted2 = sink
        .submit(ExecutorId("e1".into()), 2, ReviewDecision::Approve)
        .await;
    assert!(submitted2);
    let decision = rx.await.expect("decision");
    assert!(matches!(decision, ReviewDecision::Approve));
}

#[tokio::test]
async fn unknown_executor_id_is_dropped() {
    let sink = ReviewSink::new();
    let submitted = sink
        .submit(ExecutorId("unknown".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(!submitted);
}

#[tokio::test]
async fn remove_cleans_up_entry() {
    let sink = ReviewSink::new();
    let _handle = sink
        .register_handle(ExecutorId("e1".into()), 1)
        .await
        .expect("registered");
    sink.remove(&ExecutorId("e1".into())).await;
    let submitted = sink
        .submit(ExecutorId("e1".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(!submitted);
}

#[tokio::test]
async fn double_register_fails() {
    let sink = ReviewSink::new();
    let _handle1 = sink
        .register_handle(ExecutorId("e1".into()), 1)
        .await
        .expect("first");
    let second = sink.register_handle(ExecutorId("e1".into()), 2).await;
    assert!(second.is_err(), "must not overwrite active entry");
}
