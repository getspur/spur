use spur_bot::telegram::poll_loop::advance_offset;
use spur_bot::telegram::router::TelegramInput;

#[test]
fn offset_advances_only_after_accepted_batch() {
    assert_eq!(advance_offset(100, &[101, 102], true), 103);
    assert_eq!(advance_offset(100, &[101, 102], false), 100);
}

#[test]
fn try_send_on_full_channel_preserves_offset_and_does_not_panic() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<TelegramInput>>(1);

    // Fill the channel with one batch
    tx.try_send(vec![TelegramInput::Text {
        user_id: 1,
        chat_id: 1,
        text: "first".into(),
    }])
    .unwrap();

    // Attempting to send a second batch should fail (not panic)
    let result = tx.try_send(vec![TelegramInput::Text {
        user_id: 1,
        chat_id: 1,
        text: "second".into(),
    }]);
    assert!(result.is_err(), "try_send should fail on a full channel");

    // When batch enqueue fails, offset must not advance
    assert_eq!(advance_offset(100, &[101], false), 100);

    // Clean up
    let _ = rx.try_recv();
}

#[test]
fn batch_forward_is_atomic_under_channel_pressure() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<TelegramInput>>(1);

    // First batch occupies the only slot.
    let batch_a = vec![
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            text: "a".into(),
        },
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            text: "b".into(),
        },
    ];
    tx.try_send(batch_a).unwrap();

    // Second batch must fail atomically: either all items are accepted or none.
    let batch_b = vec![
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            text: "c".into(),
        },
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            text: "d".into(),
        },
    ];
    let result = tx.try_send(batch_b);
    assert!(
        result.is_err(),
        "try_send of a full batch must fail atomically"
    );

    // Only batch_a was enqueued.
    let received = rx.try_recv().unwrap();
    assert_eq!(received.len(), 2);
    assert!(rx.try_recv().is_err());
}
