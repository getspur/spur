//! Pure function that turns an agent-advertised `AvailableCommand` plus
//! its owning agent's `CommandsConfig` into a `CommandEntry`. Replaces
//! the old `registry::agent_entry()` which hardcoded `if handle == "kiro"`.
//!
//! Pure (no I/O, no state) so it can be unit-tested in isolation.

use spur_acp::{AvailableCommand, AvailableCommandInput, CommandsConfig, DispatchKind};

use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

pub fn build_entry(handle: &str, cfg: &CommandsConfig, cmd: &AvailableCommand) -> CommandEntry {
    let hint = match &cmd.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };

    // Some agents (kiro-cli) advertise names with a leading slash ("/agent").
    // Strip it for display/resolve; preserve the original for VendorExec wire calls.
    let display_name = cmd.name.strip_prefix('/').unwrap_or(&cmd.name).to_string();

    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", display_name),
        },
        DispatchKind::VendorExec => {
            // Validator guarantees exec_method is present for vendor_exec
            // before we ever reach this path. Panic is correct here — it
            // indicates a missed validation bug, not a user error.
            let method = cfg
                .exec_method
                .clone()
                .expect("validator guarantees exec_method for vendor_exec");
            Dispatch::VendorExec {
                method,
                command: cmd.name.clone(),
                args_template: cfg.args_template,
            }
        }
    };

    let arg_picker_spec = spur_acp::adapter::arg_picker_hint::parse(cmd);

    CommandEntry {
        name: display_name,
        description: cmd.description.clone(),
        hint,
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
        arg_picker_spec,
    }
}

/// Like `build_entry` but sourced from a config-declared
/// `StaticCommandDecl`. The dispatch is derived from the parent
/// `CommandsConfig` — static decls inherit dispatch semantics from their
/// agent's `[commands]` block.
pub fn build_static_entry(
    handle: &str,
    cfg: &CommandsConfig,
    decl: &spur_acp::StaticCommandDecl,
) -> CommandEntry {
    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", decl.name),
        },
        DispatchKind::VendorExec => {
            let method = cfg
                .exec_method
                .clone()
                .expect("validator guarantees exec_method for vendor_exec");
            Dispatch::VendorExec {
                method,
                command: decl.name.clone(),
                args_template: cfg.args_template,
            }
        }
    };

    CommandEntry {
        name: decl.name.clone(),
        description: decl.description.clone(),
        hint: decl.hint.clone(),
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
        arg_picker_spec: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{ArgsTemplateKind, CommandsConfig, DispatchKind};

    fn cmd(name: &str) -> AvailableCommand {
        AvailableCommand::new(name, "desc")
    }

    #[test]
    fn prompt_text_config_builds_prompt_text_dispatch() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let entry = build_entry("claude", &cfg, &cmd("compact"));
        match entry.dispatch {
            Dispatch::PromptText { normalized } => assert_eq!(normalized, "/compact"),
            other => panic!("expected PromptText, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "validator guarantees exec_method for vendor_exec")]
    fn vendor_exec_without_exec_method_panics() {
        // Task 6's validator is the real guard; this test documents the
        // current contract and protects against someone removing the expect.
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: None,
            ..Default::default()
        };
        build_entry("kiro", &cfg, &cmd("foo"));
    }

    #[test]
    fn vendor_exec_config_builds_vendor_exec_dispatch() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = build_entry("kiro", &cfg, &cmd("context"));
        match entry.dispatch {
            Dispatch::VendorExec {
                method,
                command,
                args_template,
            } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(command, "context");
                assert_eq!(args_template, ArgsTemplateKind::RawRest);
            }
            other => panic!("expected VendorExec, got {other:?}"),
        }
    }

    #[test]
    fn build_static_entry_prompt_text_dispatch() {
        use spur_acp::StaticCommandDecl;
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let decl = StaticCommandDecl {
            name: "compact".into(),
            description: "Compact history".into(),
            hint: None,
        };
        let entry = build_static_entry("codex", &cfg, &decl);
        assert_eq!(entry.name, "compact");
        assert_eq!(entry.description, "Compact history");
        match entry.dispatch {
            Dispatch::PromptText { normalized } => assert_eq!(normalized, "/compact"),
            other => panic!("expected PromptText, got {other:?}"),
        }
    }

    #[test]
    fn build_static_entry_vendor_exec_dispatch() {
        use spur_acp::{ArgsTemplateKind, StaticCommandDecl};
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let decl = StaticCommandDecl {
            name: "help".into(),
            description: "Help".into(),
            hint: None,
        };
        let entry = build_static_entry("kiro", &cfg, &decl);
        match entry.dispatch {
            Dispatch::VendorExec {
                method,
                command,
                args_template,
            } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(command, "help");
                assert_eq!(args_template, ArgsTemplateKind::RawRest);
            }
            other => panic!("expected VendorExec, got {other:?}"),
        }
    }

    #[test]
    fn build_entry_auto_derives_arg_picker_spec_for_unstructured_input() {
        use spur_acp::UnstructuredCommandInput;
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let cmd = AvailableCommand::new("review-branch", "Review against branch").input(
            AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("branch name")),
        );
        let entry = build_entry("codex", &cfg, &cmd);
        let spec = entry
            .arg_picker_spec
            .expect("Unstructured input must auto-derive an ArgPickerSpec");
        assert_eq!(spec.free_text_hint, "branch name");
        assert!(
            spec.typed_hint.is_none(),
            "PR-3 only reads the free-text hint"
        );
    }

    #[test]
    fn build_entry_no_input_yields_no_arg_picker_spec() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let cmd = AvailableCommand::new("init", "Create AGENTS.md");
        let entry = build_entry("codex", &cfg, &cmd);
        assert!(
            entry.arg_picker_spec.is_none(),
            "no-input commands must not get an arg picker"
        );
    }

    #[test]
    fn build_static_entry_preserves_hint() {
        use spur_acp::StaticCommandDecl;
        let cfg = CommandsConfig::default();
        let decl = StaticCommandDecl {
            name: "model".into(),
            description: "Switch model".into(),
            hint: Some("[name]".into()),
        };
        let entry = build_static_entry("codex", &cfg, &decl);
        assert_eq!(entry.hint.as_deref(), Some("[name]"));
    }
}
