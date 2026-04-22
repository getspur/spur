use frankenstein::types::{Chat, ChatType, Message, User, CallbackQuery, MaybeInaccessibleMessage};
use frankenstein::updates::{Update, UpdateContent};
use spur_bot::telegram::router::{normalize_update, TelegramInput};

#[test]
fn router_rejects_non_private_updates() {
    let update = Update {
        update_id: 1,
        content: UpdateContent::Message(Box::new(
            Message::builder()
                .message_id(1)
                .date(0)
                .chat(
                    Chat::builder()
                        .id(99)
                        .type_field(ChatType::Supergroup)
                        .build(),
                )
                .text("hello")
                .build(),
        )),
    };

    assert!(normalize_update(&update, 424242).is_none());
}

#[test]
fn router_maps_private_command_text() {
    let update = Update {
        update_id: 2,
        content: UpdateContent::Message(Box::new(
            Message::builder()
                .message_id(2)
                .date(0)
                .chat(
                    Chat::builder()
                        .id(10_001)
                        .type_field(ChatType::Private)
                        .build(),
                )
                .from(
                    User::builder()
                        .id(424242)
                        .is_bot(false)
                        .first_name("Kevin")
                        .build(),
                )
                .text("/current")
                .build(),
        )),
    };

    assert!(matches!(
        normalize_update(&update, 424242),
        Some(TelegramInput::Text { chat_id: 10_001, text, .. }) if text == "/current"
    ));
}

fn test_update_with_message_thread(
    user_id: u64,
    chat_id: i64,
    message_thread_id: Option<i32>,
    text: &str,
) -> Update {
    let mut json = serde_json::json!({
        "message_id": 3,
        "date": 0,
        "chat": { "id": chat_id, "type": "private" },
        "from": { "id": user_id, "is_bot": false, "first_name": "Kevin" },
        "text": text,
    });
    if let Some(thread_id) = message_thread_id {
        json["message_thread_id"] = serde_json::json!(thread_id);
    }
    let message: Message = serde_json::from_value(json).unwrap();
    Update {
        update_id: 3,
        content: UpdateContent::Message(Box::new(message)),
    }
}

fn test_callback_update_with_thread(
    user_id: u64,
    chat_id: i64,
    message_thread_id: Option<i32>,
    query_id: &str,
    token: &str,
) -> Update {
    let mut json = serde_json::json!({
        "message_id": 4,
        "date": 0,
        "chat": { "id": chat_id, "type": "private" },
        "text": "prompt",
    });
    if let Some(thread_id) = message_thread_id {
        json["message_thread_id"] = serde_json::json!(thread_id);
    }
    let message: Message = serde_json::from_value(json).unwrap();

    let query = CallbackQuery {
        id: query_id.into(),
        from: User::builder()
            .id(user_id)
            .is_bot(false)
            .first_name("Kevin")
            .build(),
        message: Some(MaybeInaccessibleMessage::Message(Box::new(message))),
        inline_message_id: None,
        chat_instance: "ci-1".into(),
        data: Some(token.into()),
        game_short_name: None,
    };

    Update {
        update_id: 4,
        content: UpdateContent::CallbackQuery(Box::new(query)),
    }
}

#[test]
fn private_topic_message_preserves_non_general_thread_id() {
    let update = test_update_with_message_thread(338086459, 9001, Some(77), "hello");
    let input = normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        TelegramInput::Text {
            chat_id: 9001,
            message_thread_id: Some(77),
            text,
            ..
        } if text == "hello"
    ));
}

#[test]
fn general_topic_normalizes_to_lobby() {
    let update = test_update_with_message_thread(338086459, 9001, Some(1), "hello");
    let input = normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        TelegramInput::Text {
            message_thread_id: None,
            ..
        }
    ));
}

#[test]
fn callback_uses_message_thread_id_from_callback_message() {
    let update = test_callback_update_with_thread(338086459, 9001, Some(88), "cb-1", "tok-1");
    let input = normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        TelegramInput::Callback {
            chat_id: 9001,
            message_thread_id: Some(88),
            query_id,
            token,
            ..
        } if query_id == "cb-1" && token == "tok-1"
    ));
}
