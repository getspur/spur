#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Start,
    Help,
    New,
    Sessions,
    Resume { session_id: String },
    Current,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedChatInput {
    Command(BotCommand),
    PlainText(String),
}

pub fn parse_chat_input(raw: &str) -> ParsedChatInput {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("/resume ") {
        return ParsedChatInput::Command(BotCommand::Resume {
            session_id: rest.trim().to_string(),
        });
    }

    match trimmed {
        "/start" => ParsedChatInput::Command(BotCommand::Start),
        "/help" => ParsedChatInput::Command(BotCommand::Help),
        "/new" => ParsedChatInput::Command(BotCommand::New),
        "/sessions" => ParsedChatInput::Command(BotCommand::Sessions),
        "/current" => ParsedChatInput::Command(BotCommand::Current),
        "/cancel" => ParsedChatInput::Command(BotCommand::Cancel),
        _ => ParsedChatInput::PlainText(trimmed.to_string()),
    }
}
