use spur_bot::commands::{parse_chat_input, BotCommand, ParsedChatInput};

#[test]
fn parse_resume_command() {
    assert_eq!(
        parse_chat_input("/resume acp_123"),
        ParsedChatInput::Command(BotCommand::Resume {
            session_id: "acp_123".into(),
        })
    );
}

#[test]
fn plain_text_stays_plain_text() {
    assert_eq!(
        parse_chat_input("investigate review loop"),
        ParsedChatInput::PlainText("investigate review loop".into())
    );
}

#[test]
fn plain_text_preserves_leading_and_trailing_whitespace() {
    assert_eq!(
        parse_chat_input("  hello world  "),
        ParsedChatInput::PlainText("  hello world  ".into())
    );
}

#[test]
fn command_matching_tolerates_surrounding_whitespace() {
    assert_eq!(
        parse_chat_input("  /new  "),
        ParsedChatInput::Command(BotCommand::New)
    );
}
