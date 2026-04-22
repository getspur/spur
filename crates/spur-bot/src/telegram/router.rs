#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramInput {
    Text {
        user_id: i64,
        chat_id: i64,
        text: String,
    },
    Callback {
        user_id: i64,
        chat_id: i64,
        query_id: String,
        token: String,
    },
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
                text: message.text.clone()?,
            })
        }
        frankenstein::updates::UpdateContent::CallbackQuery(query) => {
            let user = &query.from;
            if user.id as i64 != operator_user_id {
                return None;
            }
            let chat_id = match query.message.as_ref()? {
                frankenstein::types::MaybeInaccessibleMessage::Message(msg) => msg.chat.id,
                frankenstein::types::MaybeInaccessibleMessage::InaccessibleMessage(msg) => {
                    msg.chat.id
                }
            };
            Some(TelegramInput::Callback {
                user_id: user.id as i64,
                chat_id,
                query_id: query.id.clone(),
                token: query.data.clone()?,
            })
        }
        _ => None,
    }
}
