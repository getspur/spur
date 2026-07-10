#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramInput {
    Text {
        user_id: i64,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
    },
    Callback {
        user_id: i64,
        chat_id: i64,
        message_thread_id: Option<i32>,
        query_id: String,
        token: String,
    },
}

fn normalize_thread_id(thread_id: Option<i32>) -> Option<i32> {
    match thread_id {
        Some(1) | None => None,
        Some(other) => Some(other),
    }
}

pub fn normalize_update(
    update: &frankenstein::updates::Update,
    operator_user_id: i64,
) -> Option<TelegramInput> {
    match &update.content {
        frankenstein::updates::UpdateContent::Message(message)
            if message.chat.type_field == frankenstein::types::ChatType::Private =>
        {
            let user = message.from.as_ref()?;
            if user.id as i64 != operator_user_id {
                return None;
            }
            Some(TelegramInput::Text {
                user_id: user.id as i64,
                chat_id: message.chat.id,
                message_thread_id: normalize_thread_id(message.message_thread_id),
                text: message.text.clone()?,
            })
        }
        frankenstein::updates::UpdateContent::CallbackQuery(query) => {
            let user = &query.from;
            if user.id as i64 != operator_user_id {
                return None;
            }
            let (chat_id, message_thread_id) = match query.message.as_ref()? {
                frankenstein::types::MaybeInaccessibleMessage::Message(msg) => {
                    (msg.chat.id, normalize_thread_id(msg.message_thread_id))
                }
                frankenstein::types::MaybeInaccessibleMessage::InaccessibleMessage(msg) => {
                    (msg.chat.id, None)
                }
            };
            Some(TelegramInput::Callback {
                user_id: user.id as i64,
                chat_id,
                message_thread_id,
                query_id: query.id.clone(),
                token: query.data.clone()?,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankenstein::types::{
        CallbackQuery, Chat, ChatType, MaybeInaccessibleMessage, Message, User,
    };
    use frankenstein::updates::{Update, UpdateContent};

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
}
