use spur_bot::telegram::sender::{DraftUpdate, TelegramSender};

#[tokio::test(start_paused = true)]
async fn sender_coalesces_draft_updates_by_draft_id() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            message_thread_id: None,
            draft_id: "draft-1".into(),
            text: "alpha".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            message_thread_id: None,
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
            message_thread_id: None,
            draft_id: "draft-1".into(),
            text: "first".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            message_thread_id: None,
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

#[tokio::test(start_paused = true)]
async fn sender_coalesces_by_chat_and_thread() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 42,
            message_thread_id: Some(7),
            draft_id: "draft-a".into(),
            text: "first".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 42,
            message_thread_id: Some(8),
            draft_id: "draft-a".into(),
            text: "second".into(),
        })
        .await;

    tokio::time::advance(std::time::Duration::from_millis(500)).await;
    let first = rx.recv().await.unwrap();
    let second = rx.recv().await.unwrap();

    assert_ne!(first.message_thread_id, second.message_thread_id);
}

#[tokio::test(start_paused = true)]
async fn sender_does_not_collide_same_draft_thread_across_chats() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 100,
            message_thread_id: Some(7),
            draft_id: "draft-a".into(),
            text: "from chat 100".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 200,
            message_thread_id: Some(7),
            draft_id: "draft-a".into(),
            text: "from chat 200".into(),
        })
        .await;

    tokio::time::advance(std::time::Duration::from_millis(500)).await;

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for first draft")
        .expect("channel closed");
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for second draft")
        .expect("channel closed");

    // Both drafts should be preserved, not coalesced.
    let texts = std::collections::HashSet::from([first.text.as_str(), second.text.as_str()]);
    assert!(
        texts.contains("from chat 100"),
        "expected draft from chat 100"
    );
    assert!(
        texts.contains("from chat 200"),
        "expected draft from chat 200"
    );
}
