use agent_client_protocol::schema::{ToolCall, ToolKind};
use serde_json::json;
use spur_acp::{
    adapter::{self, mcp::unwrap_envelope, ObservePayload, ToolFamily},
    AgentKind,
};

// ─── classify_tool ──────────────────────────────────────────────────────────

#[test]
fn read_tool_kind_classifies_as_read() {
    let tc = ToolCall::new("tc-1", "read_file").kind(ToolKind::Read);
    assert_eq!(
        adapter::classify_tool(&tc, AgentKind::Generic),
        ToolFamily::Read
    );
}

#[test]
fn mcp_title_classifies_as_mcp() {
    let tc = ToolCall::new("tc-2", "mcp__srv__foo").kind(ToolKind::Other);
    assert_eq!(
        adapter::classify_tool(&tc, AgentKind::Generic),
        ToolFamily::Mcp
    );
}

#[test]
fn todo_write_title_classifies_as_plan() {
    let tc = ToolCall::new("tc-3", "TodoWrite").kind(ToolKind::Other);
    assert_eq!(
        adapter::classify_tool(&tc, AgentKind::Generic),
        ToolFamily::Plan
    );
}

#[test]
fn plan_update_title_classifies_as_plan_for_generic() {
    let tc = ToolCall::new("tc-4", "plan_update").kind(ToolKind::Other);
    assert_eq!(
        adapter::classify_tool(&tc, AgentKind::Generic),
        ToolFamily::Plan
    );
}

#[test]
fn execute_kind_classifies_as_execute() {
    let tc = ToolCall::new("tc-5", "bash").kind(ToolKind::Execute);
    assert_eq!(
        adapter::classify_tool(&tc, AgentKind::ClaudeCodeAcp),
        ToolFamily::Execute
    );
}

// ─── extract_observe ────────────────────────────────────────────────────────

#[test]
fn extract_observe_string_returns_text() {
    let v = json!("hello");
    let result = adapter::extract_observe(&v, AgentKind::Generic);
    match result {
        ObservePayload::Text { body } => assert_eq!(body, "hello"),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn extract_observe_command_output() {
    let v = json!({"exit_code": 0, "stdout": "ok", "stderr": ""});
    let result = adapter::extract_observe(&v, AgentKind::Generic);
    match result {
        ObservePayload::CommandOutput {
            exit_code,
            stdout,
            stderr,
        } => {
            assert_eq!(exit_code, Some(0));
            assert_eq!(stdout, "ok");
            assert_eq!(stderr, "");
        }
        other => panic!("expected CommandOutput, got {:?}", other),
    }
}

#[test]
fn extract_observe_null_returns_empty_text() {
    let v = json!(null);
    let result = adapter::extract_observe(&v, AgentKind::Generic);
    match result {
        ObservePayload::Text { body } => assert!(body.is_empty()),
        other => panic!("expected Text, got {:?}", other),
    }
}

// ─── unwrap_envelope (integration checks in smoke layer) ────────────────────

#[test]
fn unwrap_single_json_envelope() {
    let v = json!({"items": [{"Json": {"key": "value"}}]});
    let result = unwrap_envelope(&v);
    assert_eq!(*result, json!({"key": "value"}));
}

#[test]
fn unwrap_multi_text_envelope() {
    let v = json!({"items": [{"Text": "first"}, {"Text": "second"}]});
    let result = unwrap_envelope(&v);
    assert_eq!(*result, json!("first\nsecond"));
}

#[test]
fn unwrap_zero_items_passthrough() {
    let v = json!({"items": []});
    let result = unwrap_envelope(&v);
    assert_eq!(*result, v);
}

#[test]
fn mode_badge_generic_returns_none() {
    assert!(adapter::mode_badge("plan", AgentKind::Generic).is_none());
}

// ─── format_input ───────────────────────────────────────────────────────────

#[test]
fn format_input_path_field() {
    use spur_acp::adapter::ToolInputDisplay;
    let v = json!({"path": "/tmp/foo.rs"});
    let result = adapter::format_input(&v, AgentKind::Generic);
    match result {
        ToolInputDisplay::Path(p) => assert_eq!(p, "/tmp/foo.rs"),
        other => panic!("expected Path, got {:?}", other),
    }
}

#[test]
fn format_input_command_field() {
    use spur_acp::adapter::ToolInputDisplay;
    let v = json!({"command": "cargo test", "cwd": "/tmp"});
    let result = adapter::format_input(&v, AgentKind::Generic);
    match result {
        ToolInputDisplay::Command { cmd, cwd } => {
            assert_eq!(cmd, "cargo test");
            assert_eq!(cwd, Some("/tmp".to_string()));
        }
        other => panic!("expected Command, got {:?}", other),
    }
}

#[test]
fn codex_harmony_command_array_formats_as_command_input() {
    let raw = json!({
        "command": ["/bin/zsh", "-lc", "sed -n '1,180p' AGENTS.md"],
        "cwd": "/Volumes/Projects/spur",
        "source": "unified_exec_startup"
    });

    let display = adapter::format_input(&raw, AgentKind::CodexAcp);

    match display {
        adapter::ToolInputDisplay::Command { cmd, cwd } => {
            assert_eq!(cmd, "sed -n '1,180p' AGENTS.md");
            assert_eq!(cwd.as_deref(), Some("/Volumes/Projects/spur"));
        }
        other => panic!("expected Command display for Codex harmony input, got {other:?}"),
    }
}
