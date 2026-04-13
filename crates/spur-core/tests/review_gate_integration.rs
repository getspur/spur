use spur_acp::ReviewDecision;
use spur_core::{review_dispatcher_loop, ExecutorId, InteractiveInput, ReviewSink};

#[tokio::test]
async fn dispatcher_routes_submit_review_to_sink() {
    let sink = ReviewSink::new();
    let rx = sink
        .register(ExecutorId::new("e1"), 1)
        .await
        .expect("registered");
    let (tx, input_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(4);

    let sink_for_task = sink.clone();
    let handle = tokio::spawn(review_dispatcher_loop(input_rx, sink_for_task));

    tx.send(InteractiveInput::SubmitReview {
        executor_id: "e1".into(),
        attempt_n: 1,
        decision: ReviewDecision::Approve,
    })
    .await
    .unwrap();

    let decision = rx.await.expect("decision delivered");
    assert!(matches!(decision, ReviewDecision::Approve));

    drop(tx);
    handle.await.unwrap();
}

#[tokio::test]
async fn dispatcher_ignores_non_review_variants() {
    // Sends Message / ListSessions / etc into the dispatcher channel;
    // assert the ReviewSink has no registered entry and the dispatcher
    // does not panic.
    let sink = ReviewSink::new();
    let (tx, input_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(4);
    let handle = tokio::spawn(review_dispatcher_loop(input_rx, sink.clone()));

    tx.send(InteractiveInput::Message {
        text: "hi".into(),
        interrupt: false,
    })
    .await
    .unwrap();
    tx.send(InteractiveInput::ListSessions).await.unwrap();

    // Give the dispatcher a chance to process and ignore.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    drop(tx);
    handle.await.unwrap();
    // No assertion beyond "did not panic" + "handle completed".
}
