use spur_bot::telegram::sender::{DraftUpdate, TelegramSender};

#[tokio::test(start_paused = true)]
async fn sender_coalesces_draft_updates_by_draft_id() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "alpha".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "alpha beta".into(),
        })
        .await;

    tokio::time::advance(std::time::Duration::from_millis(500)).await;

    let sent = rx.recv().await.unwrap();
    assert_eq!(sent.text, "alpha beta");
}

#[tokio::test(start_paused = true)]
async fn sender_delays_first_flush_until_interval_elapses() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "first".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "second".into(),
        })
        .await;

    // Yield so the background task enters its select loop, but do not
    // advance the clock past the 400 ms coalescing window.
    tokio::task::yield_now().await;

    // If the interval ticks immediately (the bug), the coalesced draft
    // would already be in the channel.
    assert!(
        rx.try_recv().is_err(),
        "draft must not be flushed before flush_every elapses"
    );

    // Advance past the 400 ms window so the interval fires.
    tokio::time::advance(std::time::Duration::from_millis(500)).await;

    let sent = rx.recv().await.unwrap();
    assert_eq!(sent.text, "second");
}
