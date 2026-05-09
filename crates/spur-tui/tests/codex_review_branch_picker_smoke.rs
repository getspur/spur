//! End-to-end smoke for the v2 PR-3 free-text picker against codex's
//! `/review-branch` advertised command.
//!
//! Composes the public surfaces in one happy path so a regression in
//! any of `arg_picker_hint::parse`, `agents::build_entry` auto-derivation,
//! `CommandRegistry::set_agent_commands` + `arg_picker_spec` lookup, or
//! `submit_router::route` (PromptText path with arg) surfaces here.
//!
//! Wire reality: codex-acp 0.12.0 emits an `available_commands_update`
//! notification with /review-branch carrying `input.hint = "branch name"`
//! (verified in `crates/spur-acp/tests/codex_0_12_wire_probe.rs`).

use spur_acp::{
    AvailableCommand, AvailableCommandInput, CommandsConfig, DispatchKind, UnstructuredCommandInput,
};
use spur_tui::commands::submit_router::{route, SubmitDecision};
use spur_tui::commands::CommandRegistry;

const HANDLE: &str = "codex";

fn codex_advertised_commands() -> Vec<AvailableCommand> {
    // Mirrors the captured payload from
    // crates/spur-acp/tests/data/codex_acp_0_12_new_session_response.json
    // (available_commands_update notification body), minus the cache-only
    // metadata.
    vec![
        AvailableCommand::new("review", "Review my current changes and find issues").input(
            AvailableCommandInput::Unstructured(UnstructuredCommandInput::new(
                "optional custom review instructions",
            )),
        ),
        AvailableCommand::new(
            "review-branch",
            "Review the code changes against a specific branch",
        )
        .input(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new("branch name"),
        )),
        AvailableCommand::new(
            "review-commit",
            "Review the code changes introduced by a commit",
        )
        .input(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new("commit sha"),
        )),
        AvailableCommand::new(
            "init",
            "create an AGENTS.md file with instructions for Codex",
        ),
        AvailableCommand::new(
            "compact",
            "summarize conversation to prevent hitting the context limit",
        ),
        AvailableCommand::new("undo", "undo Codex's most recent turn"),
        AvailableCommand::new("logout", "logout of Codex"),
    ]
}

#[test]
fn codex_review_branch_picker_end_to_end() {
    let cfg = CommandsConfig {
        dispatch: DispatchKind::PromptText,
        ..Default::default()
    };

    // 1. Build the registry from the agent-advertised commands.
    let entries: Vec<_> = codex_advertised_commands()
        .iter()
        .map(|cmd| spur_tui::agents::build_entry(HANDLE, &cfg, cmd))
        .collect();

    // Sanity: only the 3 input-bearing commands carry a picker spec.
    let with_spec: Vec<&str> = entries
        .iter()
        .filter(|e| e.arg_picker_spec.is_some())
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        with_spec,
        vec!["review", "review-branch", "review-commit"],
        "only Unstructured-input commands must auto-derive an ArgPickerSpec"
    );

    let mut registry = CommandRegistry::new();
    registry.set_agent_commands(HANDLE, entries);

    // 2. Registry exposes the spec by name (consumed by InputCompletionPort).
    let spec = registry
        .arg_picker_spec("review-branch")
        .expect("/review-branch must expose an ArgPickerSpec");
    assert_eq!(spec.free_text_hint, "branch name");
    assert!(
        spec.typed_hint.is_none(),
        "PR-3 reads only the free-text hint; PR-4 will add _meta typed_hint"
    );

    // /init has no input — must not surface a picker spec.
    assert!(
        registry.arg_picker_spec("init").is_none(),
        "no-input commands must not surface a picker"
    );

    // 3. Submit "/review-branch main" → assembles canonical text and routes
    //    as PromptText. This is the wire shape codex's parser already
    //    understands (verified at codex-acp/src/thread.rs:2735-2767).
    match route("/review-branch main", &[], &[], &registry, false) {
        SubmitDecision::Send { blocks, interrupt } => {
            assert!(!interrupt);
            use agent_client_protocol::schema::ContentBlock;
            assert_eq!(blocks.len(), 1);
            let text = match &blocks[0] {
                ContentBlock::Text(t) => &t.text,
                other => panic!("expected Text, got {other:?}"),
            };
            assert_eq!(text, "/review-branch main");
        }
        other => panic!("expected Send, got {other:?}"),
    }

    // 4. Empty arg ("/review-branch ") still routes as Send (PromptText)
    //    so the agent gets a chance to respond — codex itself decides
    //    whether to require an arg.
    match route("/review-branch", &[], &[], &registry, false) {
        SubmitDecision::Send { blocks, .. } => {
            use agent_client_protocol::schema::ContentBlock;
            let text = match &blocks[0] {
                ContentBlock::Text(t) => &t.text,
                other => panic!("expected Text, got {other:?}"),
            };
            assert_eq!(text, "/review-branch");
        }
        other => panic!("expected Send, got {other:?}"),
    }
}
