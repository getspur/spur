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
