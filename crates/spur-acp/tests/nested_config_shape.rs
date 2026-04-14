//! Spec 1: parse `[agents.entries.commands]`, `[agents.entries.permissions]`,
//! and `[agents.entries.display]` sub-tables.

use spur_acp::config::{
    AgentConfig, ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind,
    ResponseRenderKind,
};

#[test]
fn parses_prompt_text_commands_block() {
    let toml_src = r#"
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[display]
handle = "claude"
display_name = "Claude"

[commands]
dispatch = "prompt_text"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.display.handle.as_deref(), Some("claude"));
    assert_eq!(cfg.commands.dispatch, DispatchKind::PromptText);
    assert!(cfg.commands.exec_method.is_none());
    assert!(cfg.commands.ingest.is_empty());
    assert!(cfg.commands.response.is_empty());
}

#[test]
fn parses_vendor_exec_commands_block() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

[[commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"

[[commands.response]]
method = "_kiro.dev/commands/execute"
render = "system_note"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.commands.dispatch, DispatchKind::VendorExec);
    assert_eq!(
        cfg.commands.exec_method.as_deref(),
        Some("_kiro.dev/commands/execute")
    );
    assert_eq!(cfg.commands.args_template, ArgsTemplateKind::RawRest);
    assert_eq!(cfg.commands.ingest.len(), 1);
    assert_eq!(cfg.commands.ingest[0].parser, IngestParserKind::JsonPathList);
    assert_eq!(cfg.commands.ingest[0].item_schema, ItemSchemaKind::AcpAvailableCommand);
    assert_eq!(cfg.commands.response.len(), 1);
    assert_eq!(cfg.commands.response[0].render, ResponseRenderKind::SystemNote);
}

#[test]
fn unknown_dispatch_kind_is_rejected() {
    let toml_src = r#"
name = "bogus"
command = "bogus-cli"
transport = "acp"

[commands]
dispatch = "teleport"
"#;
    let err = toml::from_str::<AgentConfig>(toml_src).expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("teleport") || msg.contains("unknown variant"),
        "expected unknown-variant error, got: {msg}"
    );
}

#[test]
fn permissions_nested_block_parses() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[permissions]
skip = true
args = ["--trust-all-tools"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.permissions.skip);
    assert_eq!(cfg.permissions.args, vec!["--trust-all-tools".to_string()]);
    assert!(cfg.permissions.session_mode.is_none());
}
