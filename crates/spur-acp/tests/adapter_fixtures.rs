use agent_client_protocol::schema::v1::{SessionNotification, SessionUpdate};
use spur_acp::{
    adapter::{self, ObservePayload, ToolFamily},
    AgentKind,
};

fn load(rel: &str) -> SessionNotification {
    let path = format!(
        "{}/tests/fixtures/notifications/{}",
        env!("CARGO_MANIFEST_DIR"),
        rel
    );
    let json = std::fs::read_to_string(&path).expect("read fixture");
    serde_json::from_str(&json).expect("parse fixture")
}

// ─── claude-code-acp fixtures ───────────────────────────────────────────────

#[test]
fn claude_bash_tool_classifies_as_execute() {
    let n = load("claude-code-acp/tool_call_bash.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(
        adapter::classify_tool(tc, AgentKind::ClaudeCodeAcp),
        ToolFamily::Execute
    );
}

#[test]
fn claude_bash_update_extracts_command_output_exit0() {
    let n = load("claude-code-acp/tool_update_bash_exit0.json");
    let SessionUpdate::ToolCallUpdate(tcu) = &n.update else {
        panic!("expected ToolCallUpdate");
    };
    let raw = tcu.fields.raw_output.as_ref().expect("raw_output present");
    let p = adapter::extract_observe(raw, AgentKind::ClaudeCodeAcp);
    match p {
        ObservePayload::CommandOutput {
            exit_code: Some(0),
            ref stdout,
            ref stderr,
        } => {
            assert_eq!(stdout, "ok\n");
            assert_eq!(stderr, "");
        }
        other => panic!("expected CommandOutput exit 0, got {:?}", other),
    }
}

#[test]
fn claude_edit_tool_classifies_as_edit() {
    let n = load("claude-code-acp/tool_call_edit.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(
        adapter::classify_tool(tc, AgentKind::ClaudeCodeAcp),
        ToolFamily::Edit
    );
}

// ─── codex-acp fixtures ─────────────────────────────────────────────────────

#[test]
fn codex_exec_tool_classifies_as_execute() {
    let n = load("codex-acp/tool_call_exec.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(
        adapter::classify_tool(tc, AgentKind::CodexAcp),
        ToolFamily::Execute
    );
}

#[test]
fn codex_exec_output_extracts_as_command_output() {
    let n = load("codex-acp/tool_update_exec_exit0.json");
    let SessionUpdate::ToolCallUpdate(tcu) = &n.update else {
        panic!("expected ToolCallUpdate");
    };
    let raw = tcu.fields.raw_output.as_ref().expect("raw_output present");
    let p = adapter::extract_observe(raw, AgentKind::CodexAcp);
    match p {
        ObservePayload::CommandOutput {
            exit_code: Some(0),
            ref stdout,
            ref stderr,
        } => {
            assert_eq!(stdout, "done\n");
            assert_eq!(stderr, "");
        }
        other => panic!("expected CommandOutput exit 0, got {:?}", other),
    }
}

#[test]
fn codex_apply_patch_classifies_as_edit() {
    let n = load("codex-acp/tool_call_apply_patch.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(
        adapter::classify_tool(tc, AgentKind::CodexAcp),
        ToolFamily::Edit
    );
}

// ─── kiro fixtures ──────────────────────────────────────────────────────────

#[test]
fn kiro_mcp_tool_classifies_as_mcp() {
    let n = load("kiro/tool_call_mcp.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    assert_eq!(adapter::classify_tool(tc, AgentKind::Kiro), ToolFamily::Mcp);
}

#[test]
fn kiro_mcp_envelope_unwraps_without_panic() {
    let n = load("kiro/tool_update_mcp_envelope.json");
    let SessionUpdate::ToolCallUpdate(tcu) = &n.update else {
        panic!("expected ToolCallUpdate");
    };
    let raw = tcu.fields.raw_output.as_ref().expect("raw_output present");
    // The MCP envelope `{"items": [{"Json": {...}}]}` should unwrap to the inner Json.
    // extract_observe must not panic and should return a valid payload.
    let p = adapter::extract_observe(raw, AgentKind::Kiro);
    // After unwrap the inner value is `{"ok": true}` — no exit_code/stdout,
    // so it must fall to Json (NOT Text). A regression in extract_observe
    // that produced Text here should be caught by CI, not silently accepted.
    match p {
        ObservePayload::Json { .. } => {}
        other => panic!("expected Json payload from mcp envelope, got: {:?}", other),
    }
}

#[test]
fn kiro_generic_tool_classifies_as_unknown() {
    let n = load("kiro/tool_call_generic.json");
    let SessionUpdate::ToolCall(tc) = &n.update else {
        panic!("expected ToolCall");
    };
    // "custom-thing" has ToolKind::Other and no recognized prefix → Unknown
    assert_eq!(
        adapter::classify_tool(tc, AgentKind::Kiro),
        ToolFamily::Unknown
    );
}
