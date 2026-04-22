use spur_bot::telegram::poll_loop::advance_offset;
use spur_bot::telegram::router::TelegramInput;

#[test]
fn offset_advances_only_after_accepted_batch() {
    assert_eq!(advance_offset(100, &[101, 102], true), 103);
    assert_eq!(advance_offset(100, &[101, 102], false), 100);
}

#[test]
fn try_send_on_full_channel_preserves_offset_and_does_not_panic() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TelegramInput>(1);

    // Fill the channel
    tx.try_send(TelegramInput::Text {
        user_id: 1,
        chat_id: 1,
        text: "first".into(),
    })
    .unwrap();

    // Attempting to send a second item should fail (not panic)
    let result = tx.try_send(TelegramInput::Text {
        user_id: 1,
        chat_id: 1,
        text: "second".into(),
    });
    assert!(result.is_err(), "try_send should fail on a full channel");

    // When batch enqueue fails, offset must not advance
    assert_eq!(advance_offset(100, &[101], false), 100);

    // Clean up
    let _ = rx.try_recv();
}
