use spur_bot::telegram::poll_loop::advance_offset;
use spur_bot::telegram::router::TelegramInput;

#[test]
fn offset_advances_to_max_id_plus_one() {
    assert_eq!(advance_offset(100, &[101, 102]), 103);
    // Empty batch leaves offset unchanged.
    assert_eq!(advance_offset(100, &[]), 100);
}

#[test]
fn closed_update_channel_propagates_send_error() {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<TelegramInput>>(1);
    drop(rx);

    let result = tx.try_send(vec![TelegramInput::Text {
        user_id: 1,
        chat_id: 1,
        message_thread_id: None,
        text: "after-close".into(),
    }]);
    assert!(
        matches!(
            result,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
        ),
        "closed receiver must surface as TrySendError::Closed"
    );
}

#[test]
fn batch_forward_is_atomic_under_channel_pressure() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<TelegramInput>>(1);

    // First batch occupies the only slot.
    let batch_a = vec![
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            message_thread_id: None,
            text: "a".into(),
        },
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            message_thread_id: None,
            text: "b".into(),
        },
    ];
    tx.try_send(batch_a).unwrap();

    // Second batch must fail atomically: either all items are accepted or none.
    let batch_b = vec![
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            message_thread_id: None,
            text: "c".into(),
        },
        TelegramInput::Text {
            user_id: 1,
            chat_id: 1,
            message_thread_id: None,
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

#[tokio::test]
async fn batch_forward_preserves_thread_identity() {
    let batch = [spur_bot::telegram::router::TelegramInput::Text {
        user_id: 338086459,
        chat_id: 42,
        message_thread_id: Some(77),
        text: "hello".into(),
    }];

    assert_eq!(
        match &batch[0] {
            spur_bot::telegram::router::TelegramInput::Text {
                message_thread_id, ..
            } => *message_thread_id,
            _ => None,
        },
        Some(77)
    );
}
