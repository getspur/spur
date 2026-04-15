//! SubmitRouter — decide what to do with an Enter-submitted InputBar.
//!
//! On Enter, the `InputBar` captures `(text, ranges, interrupt)`. The
//! router takes that triple plus the `CommandRegistry` and returns a
//! `SubmitDecision`:
//!
//! * `Empty`         — nothing to do.
//! * `Send`          — forward `Vec<ContentBlock>` to the agent.
//! * `Local`         — fire an `Action` without sending a message.
//! * `VendorExec`    — invoke an agent-specific vendor extension RPC.
//!
//! Non-slash text routes to `Send`, assembling blocks by interleaving
//! `Text` with `ResourceLink` blocks from `ranges`.

use serde_json::Value;
use spur_acp::{ContentBlock, ResourceLink, TextContent};

use crate::action::Action;
use crate::components::input_bar::ProtectedRange;

use super::entry::Dispatch;
use super::registry::CommandRegistry;

/// What the controller should do with an Enter-submitted InputBar.
#[derive(Debug)]
pub enum SubmitDecision {
    Send {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    Local {
        action: Action,
    },
    /// Generic vendor-extension dispatch. Carries the full wire method and
    /// the rendered params payload — the consumer (app.rs → orchestrator)
    /// calls `connection.call_ext(method, params)`.
    VendorExec {
        method: String,
        params: Value,
    },
    Empty,
}

/// Route a submitted input to a `SubmitDecision`.
pub fn route(
    text: &str,
    ranges: &[ProtectedRange],
    registry: &CommandRegistry,
    interrupt: bool,
) -> SubmitDecision {
    if text.is_empty() {
        return SubmitDecision::Empty;
    }

    if text.starts_with('/') {
        if let Some(entry) = registry.resolve(text) {
            return match entry.dispatch {
                Dispatch::SpurLocal(action) => SubmitDecision::Local { action },
                Dispatch::PromptText { normalized } => {
                    let rest = rest_after_first_token(text);
                    let normalized_full = if rest.is_empty() {
                        normalized
                    } else {
                        format!("{} {}", normalized, rest)
                    };
                    SubmitDecision::Send {
                        blocks: vec![ContentBlock::Text(TextContent::new(normalized_full))],
                        interrupt,
                    }
                }
                Dispatch::VendorExec {
                    method,
                    command,
                    args_template,
                } => {
                    let rest = rest_after_first_token(text);
                    let params = match args_template {
                        spur_acp::ArgsTemplateKind::RawRest => {
                            if rest.is_empty() {
                                serde_json::json!({ "command": command })
                            } else {
                                serde_json::json!({
                                    "command": command,
                                    "args": { "raw": rest },
                                })
                            }
                        }
                    };
                    SubmitDecision::VendorExec { method, params }
                }
            };
        }
        // Unknown /command — fall through to Send as plain text so the
        // agent receives it (agents often render unknown slash commands
        // verbatim as prompts).
    }

    let blocks = assemble_blocks(text, ranges);
    SubmitDecision::Send { blocks, interrupt }
}

/// Everything after the first whitespace-delimited token of `text`.
fn rest_after_first_token(text: &str) -> String {
    match text.split_once(char::is_whitespace) {
        Some((_, rest)) => rest.trim_start().to_string(),
        None => String::new(),
    }
}

/// Walk `text` + sorted `ranges` interleaved → `[Text, ResourceLink, Text, …]`.
pub fn assemble_blocks(text: &str, ranges: &[ProtectedRange]) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    for r in ranges {
        if r.start > cursor {
            out.push(ContentBlock::Text(TextContent::new(
                text[cursor..r.start].to_string(),
            )));
        }
        out.push(ContentBlock::ResourceLink(ResourceLink::new(
            r.name.clone(),
            r.uri.clone(),
        )));
        cursor = r.end;
    }
    if cursor < text.len() {
        out.push(ContentBlock::Text(TextContent::new(
            text[cursor..].to_string(),
        )));
    }
    if out.is_empty() {
        out.push(ContentBlock::Text(TextContent::new(text.to_string())));
    }
    out
}

/// Flatten blocks into a human-readable string for the local trace echo.
///
/// `Text` blocks concatenate their text; `ResourceLink` blocks render as
/// `@<name>`; unknown variants are skipped.
pub fn blocks_preview(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text(t) => s.push_str(&t.text),
            ContentBlock::ResourceLink(r) => {
                s.push('@');
                s.push_str(&r.name);
            }
            _ => {}
        }
    }
    s
}

/// Flatten blocks into a plain text string (e.g. for CLI that forwards text).
/// Currently identical to `blocks_preview` — kept as a distinct entry point
/// so future divergence (e.g. CLI-specific serialization) is cheap.
pub fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks_preview(blocks)
}

#[cfg(test)]
mod sessions_slash_tests {
    use super::*;
    use crate::commands::registry::CommandRegistry;

    fn build_registry_for_test() -> CommandRegistry {
        CommandRegistry::new()
    }

    #[test]
    fn slash_sessions_routes_to_request_sessions() {
        let registry = build_registry_for_test();

        let decision = route("/sessions", &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::RequestSessions,
            } => {}
            other => panic!(
                "expected Local {{ action: RequestSessions }}, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn vendor_exec_raw_rest_produces_params_with_command_and_args() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("context", "Show context"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/context some rest", &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { method, params } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(
                    params,
                    serde_json::json!({
                        "command": "context",
                        "args": { "raw": "some rest" },
                    })
                );
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }

    #[test]
    fn vendor_exec_raw_rest_empty_args_still_includes_command() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("compact", "compact context"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/kiro:compact", &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { params, .. } => {
                assert_eq!(params, serde_json::json!({ "command": "compact" }));
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }
}
