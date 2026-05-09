//! Integration tests: a static command declared in config flows through
//! Registry → submit_router → Action with the correct shape.
//!
//! These tests exercise the full Spec 2 dispatch pipeline for both
//! prompt_text (codex-shaped, zero-Rust onboarding) and vendor_exec
//! (kiro-shaped) agents. They do not boot a real orchestrator; the
//! round-trip to the agent is covered by session_update_handling.rs.

use spur_acp::{AgentConfig, ArgsTemplateKind, CommandsConfig, DispatchKind, StaticCommandDecl};
use spur_tui::commands::submit_router::{blocks_preview, route, SubmitDecision};
use spur_tui::commands::CommandRegistry;

fn kiro_config_with_static_help() -> AgentConfig {
    let mut cfg = AgentConfig::with_defaults("kiro");
    cfg.display.handle = Some("kiro".into());
    cfg.commands = CommandsConfig {
        dispatch: DispatchKind::VendorExec,
        exec_method: Some("_kiro.dev/commands/execute".into()),
        args_template: ArgsTemplateKind::RawRest,
        static_commands: vec![StaticCommandDecl {
            name: "help".into(),
            description: "Show kiro help".into(),
            hint: None,
        }],
        ..Default::default()
    };
    cfg
}

#[test]
fn static_vendor_exec_command_routes_to_submit_decision_vendor_exec() {
    let cfg = kiro_config_with_static_help();
    let registry = CommandRegistry::from_configs(&[cfg]);

    // Static /help must be resolvable before any set_agent_commands call.
    // Use explicit kiro: prefix to select the agent command over spur-local /help.
    let decision = route("/kiro:help", &[], &[], &registry, false);
    match decision {
        SubmitDecision::VendorExec { method, params } => {
            assert_eq!(method, "_kiro.dev/commands/execute");
            assert_eq!(params, serde_json::json!({ "command": "help" }));
        }
        other => panic!("expected VendorExec, got {:?}", other),
    }
}

#[test]
fn static_prompt_text_command_routes_to_send_with_text() {
    // Codex-shaped: dispatch = prompt_text, static /compact → Send { Text("/compact") }.
    let mut cfg = AgentConfig::with_defaults("codex");
    cfg.display.handle = Some("codex".into());
    cfg.commands = CommandsConfig {
        dispatch: DispatchKind::PromptText,
        static_commands: vec![StaticCommandDecl {
            name: "compact".into(),
            description: "Compact history".into(),
            hint: None,
        }],
        ..Default::default()
    };
    let registry = CommandRegistry::from_configs(&[cfg]);

    let decision = route("/compact", &[], &[], &registry, false);
    match decision {
        SubmitDecision::Send { blocks, .. } => {
            let text = blocks_preview(&blocks);
            assert_eq!(text, "/compact");
        }
        other => panic!("expected Send, got {:?}", other),
    }
}
