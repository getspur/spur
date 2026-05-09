use agent_client_protocol::schema::{
    Content, ContentBlock, SessionNotification, SessionUpdate, TextContent, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::json;
use spur_acp::adapter::SessionUpdateNormalizer;

fn text_tool_content(text: &str) -> ToolCallContent {
    ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new(text))))
}

#[test]
fn kimi_argument_content_update_populates_raw_input_and_kind() {
    let id = "call-kimi";
    let mut normalizer = SessionUpdateNormalizer::default();
    normalizer.normalize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new(id, "ReadFile")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("")]),
        ),
    ));

    let out = normalizer.normalize(SessionNotification::new(
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
fn kimi_completed_content_update_populates_file_raw_output_from_known_input() {
    let id = "call-kimi-output";
    let mut normalizer = SessionUpdateNormalizer::default();
    normalizer.normalize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new(id, "ReadFile")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("")]),
        ),
    ));
    normalizer.normalize(SessionNotification::new(
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

    let out = normalizer.normalize(SessionNotification::new(
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
fn codex_harmony_command_array_formats_as_command_input() {
    let raw = json!({
        "command": ["/bin/zsh", "-lc", "sed -n '1,180p' AGENTS.md"],
        "cwd": "/Volumes/Projects/spur",
        "source": "unified_exec_startup"
    });

    let display = spur_acp::adapter::format_input(&raw, spur_acp::AgentKind::CodexAcp);

    match display {
        spur_acp::adapter::ToolInputDisplay::Command { cmd, cwd } => {
            assert_eq!(cmd, "sed -n '1,180p' AGENTS.md");
            assert_eq!(cwd.as_deref(), Some("/Volumes/Projects/spur"));
        }
        other => panic!("expected Command display for Codex harmony input, got {other:?}"),
    }
}
