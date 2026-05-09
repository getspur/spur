use agent_client_protocol::schema::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::json;
use spur_acp::{
    adapter::{self, ToolInputDisplay},
    AgentKind,
};

fn tool_content_text(content: &[ToolCallContent]) -> String {
    let mut out = String::new();
    for item in content {
        if let ToolCallContent::Content(content) = item {
            if let ContentBlock::Text(text) = &content.content {
                out.push_str(&text.text);
            }
        }
    }
    out
}

#[test]
fn agent_kind_recognizes_gemini() {
    assert_eq!(AgentKind::from_name("gemini"), AgentKind::Gemini);
    assert_eq!(AgentKind::from_name("gemini-acp"), AgentKind::Gemini);
    assert_eq!(AgentKind::from_name("gemini-cli"), AgentKind::Gemini);
}

#[test]
fn gemini_standardizer_populates_empty_delegate_tool_call() {
    let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Gemini);

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCall(
            ToolCall::new("invoke-agent", "Delegating to agent 'generalist'")
                .kind(ToolKind::Think)
                .status(ToolCallStatus::InProgress),
        ),
    ));

    let SessionUpdate::ToolCall(tool_call) = out.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(
        tool_call.raw_input,
        Some(json!({
            "agent": "generalist",
            "description": "Delegating to agent 'generalist'"
        }))
    );
    assert_eq!(tool_content_text(&tool_call.content), "agent: generalist");
}

#[test]
fn gemini_standardizer_populates_empty_delegate_update() {
    let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Gemini);

    let out = standardizer.standardize(SessionNotification::new(
        "session",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "invoke-agent",
            ToolCallUpdateFields::new()
                .title("Delegating to agent 'generalist'")
                .kind(ToolKind::Think)
                .status(ToolCallStatus::Completed),
        )),
    ));

    let SessionUpdate::ToolCallUpdate(update) = out.update else {
        panic!("expected ToolCallUpdate");
    };
    assert_eq!(
        update.fields.raw_input,
        Some(json!({
            "agent": "generalist",
            "description": "Delegating to agent 'generalist'"
        }))
    );
    assert_eq!(
        tool_content_text(update.fields.content.as_deref().unwrap_or_default()),
        "agent: generalist"
    );
    assert!(
        update.fields.raw_output.is_none(),
        "Gemini ACP does not expose native subagent output on invoke_agent"
    );
}

#[test]
fn gemini_delegate_input_formats_agent_name() {
    let input = json!({
        "agent": "generalist",
        "description": "Delegating to agent 'generalist'"
    });

    let display = adapter::format_input(&input, AgentKind::Gemini);

    match display {
        ToolInputDisplay::Text(text) => assert_eq!(text, "agent: generalist"),
        other => panic!("expected compact Gemini agent input, got {other:?}"),
    }
}
