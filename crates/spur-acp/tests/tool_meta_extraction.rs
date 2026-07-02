use agent_client_protocol::schema::v1::{SessionNotification, SessionUpdate};
use spur_acp::{adapter::extract_tool_meta, AgentKind};

fn load(rel: &str) -> SessionNotification {
    let path = format!(
        "{}/tests/fixtures/notifications/{}",
        env!("CARGO_MANIFEST_DIR"),
        rel
    );
    let json = std::fs::read_to_string(&path).expect("read fixture");
    serde_json::from_str(&json).expect("parse fixture")
}

#[test]
fn claude_extracts_tool_name_from_meta() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert_eq!(meta.tool_name.as_deref(), Some("Bash"));
    assert_eq!(meta.parent_tool_use_id, None);
}

#[test]
fn claude_extracts_parent_tool_use_id_from_meta() {
    let n = load("claude-code-acp/tool_call_subagent_task.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert_eq!(meta.tool_name.as_deref(), Some("Edit"));
    assert_eq!(
        meta.parent_tool_use_id.as_deref(),
        Some("tc-task-parent-001")
    );
}

#[test]
fn claude_returns_default_when_meta_absent() {
    let n = load("claude-code-acp/tool_call_bash.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert!(meta.tool_name.is_none());
    assert!(meta.parent_tool_use_id.is_none());
}

#[test]
fn generic_kind_always_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::Generic);
    assert!(meta.tool_name.is_none());
    assert!(meta.parent_tool_use_id.is_none());
}

#[test]
fn codex_stub_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::CodexAcp);
    assert!(meta.tool_name.is_none());
}

#[test]
fn kiro_stub_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::Kiro);
    assert!(meta.tool_name.is_none());
}

#[test]
fn claude_returns_default_when_claudecode_key_absent() {
    let n = load("claude-code-acp/tool_call_meta_no_claudecode.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert!(
        meta.tool_name.is_none(),
        "_meta without claudeCode key yields None"
    );
    assert!(meta.parent_tool_use_id.is_none());
}

#[test]
fn claude_ignores_nonstring_values_in_meta() {
    let n = load("claude-code-acp/tool_call_meta_nonstring_toolname.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall")
    };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert!(
        meta.tool_name.is_none(),
        "non-string toolName must be discarded"
    );
    assert!(
        meta.parent_tool_use_id.is_none(),
        "null parentToolUseId must be discarded"
    );
}
