use agent_client_protocol::schema::{
    Content, ContentBlock, SessionNotification, SessionUpdate, TextContent, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::json;
use spur_acp::{adapter, AgentKind};

fn text_tool_content(text: &str) -> ToolCallContent {
    ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new(text))))
}

#[test]
fn agent_kind_recognizes_kimi() {
    assert_eq!(AgentKind::from_name("kimi"), AgentKind::Kimi);
}

#[test]
fn kimi_standardizer_populates_raw_input_and_kind_from_streamed_arguments() {
    let id = "call-kimi";
    let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Kimi);
    standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new(id, "ReadFile")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("")]),
        ),
    ));

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("ReadFile: Cargo.toml")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content(
                    "{\"path\": \"/Volumes/Projects/spur/Cargo.toml\"}",
                )]),
        )),
    ));

    let SessionUpdate::ToolCallUpdate(update) = out.update else {
        panic!("expected ToolCallUpdate");
    };
    assert_eq!(update.fields.kind, Some(ToolKind::Read));
    assert_eq!(
        update.fields.raw_input,
        Some(json!({"path": "/Volumes/Projects/spur/Cargo.toml"}))
    );
}

#[test]
fn kimi_standardizer_populates_file_raw_output_from_known_input() {
    let id = "call-kimi-output";
    let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Kimi);
    standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new(id, "ReadFile")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("")]),
        ),
    ));
    standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("ReadFile: Cargo.toml")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content(
                    "{\"path\": \"/Volumes/Projects/spur/Cargo.toml\"}",
                )]),
        )),
    ));

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .content(vec![text_tool_content("[workspace]\nmembers = []\n")]),
        )),
    ));

    let SessionUpdate::ToolCallUpdate(update) = out.update else {
        panic!("expected ToolCallUpdate");
    };
    assert_eq!(
        update.fields.raw_output,
        Some(json!({
            "path": "/Volumes/Projects/spur/Cargo.toml",
            "content": "[workspace]\nmembers = []\n"
        }))
    );
}
