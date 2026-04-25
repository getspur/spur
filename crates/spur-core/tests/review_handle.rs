use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use spur_acp::{ReviewDecision, ReviewKind, ReviewPayload, SpurEventBody};
use spur_core::event_funnel::spawn_funnel;
use spur_core::review_sink::ReviewHandle;
use spur_core::{ExecutorId, ReviewSink};
use tokio::sync::broadcast;

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_handle_emit_routes_to_funnel_and_carries_receiver() {
    // Build a real funnel backed by a broadcast channel we can inspect.
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<spur_acp::SpurEvent>(16);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx, seq);

    let sink = ReviewSink::new();

    // register_handle is the ONLY way to get a ReviewHandle.
    let handle: ReviewHandle = sink
        .register_handle(ExecutorId::new("e1"), 1)
        .await
        .expect("register_handle should succeed");

    // Emit via the handle — this is the only emit path for ExecutorReviewRequested.
    let payload = ReviewPayload {
        summary: "test summary".to_string(),
        diff_summary: None,
        pr_url: None,
        error: None,
        delegation_plan: None,
        chosen_matches_dispatched: None,
        peer_influence: None,
    };
    handle.emit_requested(&funnel, ReviewKind::Completion, payload);

    // The funnel is async — yield to let it stamp and broadcast.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // One event captured, correct variant with correct fields.
    let event = bcast_rx.try_recv().expect("one event should be broadcast");
    assert!(
        matches!(
            &event.body,
            SpurEventBody::ExecutorReviewRequested { id, attempt_n: 1, .. }
                if id == "e1"
        ),
        "expected ExecutorReviewRequested(e1, attempt_n=1), got {:?}",
        event.body
    );

    // Nothing else in the channel.
    assert!(
        bcast_rx.try_recv().is_err(),
        "expected exactly one event in broadcast"
    );

    // Handle still owns the receiver; submit routes to it.
    let rx = handle.into_rx();
    let routed = sink
        .submit(ExecutorId::new("e1"), 1, ReviewDecision::Approve)
        .await;
    assert!(routed, "submit should route to the registered handle");
    let decision: ReviewDecision = rx.await.expect("receiver should resolve");
    assert!(
        matches!(decision, ReviewDecision::Approve),
        "expected Approve decision"
    );
}

#[tokio::test]
async fn register_handle_double_register_fails() {
    let sink = ReviewSink::new();
    let _handle = sink
        .register_handle(ExecutorId::new("e2"), 1)
        .await
        .expect("first register_handle should succeed");
    let second = sink.register_handle(ExecutorId::new("e2"), 2).await;
    assert!(second.is_err(), "double register_handle must fail");
}
