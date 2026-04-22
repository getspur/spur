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
