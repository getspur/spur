use frankenstein::types::{Chat, ChatType, Message, User};
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
                .chat(Chat::builder().id(99).type_field(ChatType::Supergroup).build())
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
                .chat(Chat::builder().id(10_001).type_field(ChatType::Private).build())
                .from(User::builder().id(424242).is_bot(false).first_name("Kevin").build())
                .text("/current")
                .build(),
        )),
    };

    assert!(
        matches!(
            normalize_update(&update, 424242),
            Some(TelegramInput::Text { chat_id: 10_001, text, .. }) if text == "/current"
        )
    );
}
