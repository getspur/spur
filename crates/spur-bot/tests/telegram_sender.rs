use spur_bot::telegram::sender::{DraftPauseState, DraftUpdate, TelegramSender};
use std::time::Duration;
use tokio::time::Instant;

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

#[tokio::test(start_paused = true)]
async fn sender_pauses_after_429_with_retry_after() {
    let pause = DraftPauseState::new();
    pause.pause_for_retry_after(Instant::now(), 13, Duration::from_millis(100));
    let (sender, mut rx) = TelegramSender::for_test_with_pause_state(20, pause);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            message_thread_id: None,
            draft_id: "draft-429".into(),
            text: "held during pause".into(),
        })
        .await;

    tokio::time::advance(Duration::from_millis(500)).await;
    tokio::task::yield_now().await;

    assert!(
        rx.try_recv().is_err(),
        "draft must not flush while retry_after pause is active"
    );
}

#[tokio::test(start_paused = true)]
async fn sender_resumes_after_pause_window_expires() {
    let pause = DraftPauseState::new();
    pause.pause_until_at_least(Instant::now() - Duration::from_millis(1));
    let (sender, mut rx) = TelegramSender::for_test_with_pause_state(20, pause);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            message_thread_id: None,
            draft_id: "draft-expired".into(),
            text: "sent after pause".into(),
        })
        .await;

    tokio::time::advance(Duration::from_millis(500)).await;

    let sent = rx.recv().await.unwrap();
    assert_eq!(sent.text, "sent after pause");
}

#[tokio::test(start_paused = true)]
async fn sender_pause_extends_monotonically_under_repeated_429s() {
    let pause = DraftPauseState::new();
    let now = Instant::now();
    let longer_pause = now + Duration::from_secs(30);
    pause.pause_until_at_least(longer_pause);

    pause.pause_for_retry_after(now, 13, Duration::from_millis(100));

    assert_eq!(pause.paused_until(), Some(longer_pause));
}

#[tokio::test(start_paused = true)]
async fn sender_coalesces_drafts_during_pause_then_sends_latest() {
    let pause = DraftPauseState::new();
    let now = Instant::now();
    pause.pause_until_at_least(now + Duration::from_secs(2));
    let (sender, mut rx) = TelegramSender::for_test_with_pause_state(20, pause);

    for text in ["first", "second", "latest"] {
        sender
            .queue_draft(DraftUpdate {
                chat_id: 10_001,
                message_thread_id: None,
                draft_id: "draft-paused".into(),
                text: text.into(),
            })
            .await;
    }

    tokio::time::advance(Duration::from_millis(500)).await;
    tokio::task::yield_now().await;
    assert!(
        rx.try_recv().is_err(),
        "draft must remain pending while pause is active"
    );

    tokio::time::advance(Duration::from_secs(2)).await;

    let sent = rx.recv().await.unwrap();
    assert_eq!(sent.text, "latest");
    assert!(
        rx.try_recv().is_err(),
        "only the latest coalesced draft should be flushed"
    );
}
