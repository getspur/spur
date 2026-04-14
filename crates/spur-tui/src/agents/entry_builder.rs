//! Pure function that turns an agent-advertised `AvailableCommand` plus
//! its owning agent's `CommandsConfig` into a `CommandEntry`. Replaces
//! the old `registry::agent_entry()` which hardcoded `if handle == "kiro"`.
//!
//! Pure (no I/O, no state) so it can be unit-tested in isolation.

use spur_acp::{AvailableCommand, AvailableCommandInput, CommandsConfig, DispatchKind};

use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

pub fn build_entry(
    handle: &str,
    cfg: &CommandsConfig,
    cmd: &AvailableCommand,
) -> CommandEntry {
    let hint = match &cmd.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };

    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", cmd.name),
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

    CommandEntry {
        name: cmd.name.clone(),
        description: cmd.description.clone(),
        hint,
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
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
            Dispatch::VendorExec { method, command, args_template } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(command, "context");
                assert_eq!(args_template, ArgsTemplateKind::RawRest);
            }
            other => panic!("expected VendorExec, got {other:?}"),
        }
    }
}
