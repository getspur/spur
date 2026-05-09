use agent_client_protocol::schema::{
    Content, ContentBlock, SessionNotification, SessionUpdate, TextContent, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::json;
use spur_acp::{
    adapter::{self, ToolInputDisplay},
    AgentKind,
};

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

#[test]
fn kimi_standardizer_preserves_native_agent_subagent_output() {
    let id = "call-kimi-agent";
    let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Kimi);
    standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new(id, "Agent")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("")]),
        ),
    ));

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("Agent: Inspect Cargo.toml workspace")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content(
                    "{\"description\":\"Inspect Cargo.toml workspace\",\"prompt\":\"Read Cargo.toml and report whether it contains a [workspace] section.\",\"subagent_type\":\"coder\"}",
                )]),
        )),
    ));

    let SessionUpdate::ToolCallUpdate(update) = out.update else {
        panic!("expected ToolCallUpdate");
    };
    assert_eq!(
        update.fields.raw_input,
        Some(json!({
            "description": "Inspect Cargo.toml workspace",
            "prompt": "Read Cargo.toml and report whether it contains a [workspace] section.",
            "subagent_type": "coder"
        }))
    );

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .content(vec![text_tool_content(
                    "agent_id: a577327c8\nresumed: false\nactual_subagent_type: coder\nstatus: completed\n\n[summary]\nYes. The file contains a `[workspace]` section starting at line 1.",
                )]),
        )),
    ));

    let SessionUpdate::ToolCallUpdate(update) = out.update else {
        panic!("expected ToolCallUpdate");
    };
    assert_eq!(
        update.fields.raw_output,
        Some(json!(
            "agent_id: a577327c8\nresumed: false\nactual_subagent_type: coder\nstatus: completed\n\n[summary]\nYes. The file contains a `[workspace]` section starting at line 1."
        ))
    );
}

#[test]
fn kimi_agent_input_formats_description_before_generic_json() {
    let input = json!({
        "description": "Inspect Cargo.toml workspace",
        "prompt": "Read Cargo.toml and report whether it contains a [workspace] section.",
        "subagent_type": "coder"
    });

    let display = adapter::format_input(&input, AgentKind::Kimi);

    match display {
        ToolInputDisplay::Text(text) => assert_eq!(text, "Inspect Cargo.toml workspace"),
        other => panic!("expected compact Kimi Agent description input, got {other:?}"),
    }
}
