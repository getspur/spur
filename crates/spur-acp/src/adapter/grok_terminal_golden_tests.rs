//! Byte-level oracle for the workaround, independent of shell execution.
//!
//! Boundary fence: the Grok adapter itself stays narrow (only POSIX-lexed
//! `/bin/bash -c|-lc <script>` wrappers normalize). Packed commands from
//! OTHER known agent kinds also stay direct-exec here. `AgentKind::Generic`
//! deliberately normalizes packed requests via `/bin/bash -c` — observed on
//! goose 1.51 — and is covered by `adapter::generic::terminal_command_tests`.

use super::normalize_terminal_command;
use crate::types::AgentKind;
use serde::Deserialize;

#[derive(Deserialize)]
struct Command {
    command: String,
    args: Vec<String>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    canonical: Command,
    packed: Vec<Command>,
}

#[derive(Deserialize)]
struct Passthrough {
    id: String,
    agent: String,
    #[serde(flatten)]
    request: Command,
}

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
    lexical_only: Vec<Case>,
    passthrough: Vec<Passthrough>,
}

fn corpus() -> Corpus {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/grok_terminal_golden.json"
    )))
    .expect("valid golden corpus")
}

#[test]
fn grok_golden_packed_scripts_preserve_exact_argv() {
    let corpus = corpus();
    assert!(!corpus.cases.is_empty());
    for case in corpus.cases.into_iter().chain(corpus.lexical_only) {
        assert!(!case.packed.is_empty(), "{}: no packed variants", case.id);
        for packed in case.packed {
            let actual = normalize_terminal_command(AgentKind::Grok, &packed.command, &packed.args)
                .unwrap_or_else(|| panic!("{}: packed wrapper was not recognized", case.id));
            assert_eq!(actual.program, case.canonical.command, "{}", case.id);
            assert_eq!(actual.args.as_slice(), case.canonical.args, "{}", case.id);

            // Verify the login flag too, without executing user startup files.
            let login = packed
                .command
                .replacen("/bin/bash -c ", "/bin/bash -lc ", 1);
            let actual = normalize_terminal_command(AgentKind::Grok, &login, &[])
                .expect("login wrapper must normalize");
            assert_eq!(actual.args[0], "-lc", "{}", case.id);
            assert_eq!(actual.args[1], case.canonical.args[1], "{}", case.id);
        }
    }
}

#[test]
fn grok_golden_passthrough_retains_original_command_args() {
    let corpus = corpus();
    assert!(!corpus.passthrough.is_empty());
    for case in corpus.passthrough {
        let kind = match case.agent.as_str() {
            "grok" => AgentKind::Grok,
            "generic" => AgentKind::Generic,
            other => panic!("unknown golden agent kind {other}"),
        };
        assert!(
            normalize_terminal_command(kind, &case.request.command, &case.request.args).is_none(),
            "{} must retain its original command/args",
            case.id
        );
    }
}

#[test]
fn packed_commands_are_unchanged_for_known_agent_kinds() {
    // Generic is excluded: packed requests route through `/bin/bash -c`
    // (goose 1.51 interop). See `adapter::generic::terminal_command_tests`.
    for kind in [
        AgentKind::ClaudeStreamJson,
        AgentKind::ClaudeCodeAcp,
        AgentKind::CodexAcp,
        AgentKind::Kiro,
        AgentKind::Kimi,
        AgentKind::Gemini,
        AgentKind::OpenCode,
        AgentKind::Pi,
    ] {
        let corpus = corpus();
        for case in corpus.cases.into_iter().chain(corpus.lexical_only) {
            for packed in case.packed {
                assert!(
                    normalize_terminal_command(kind, &packed.command, &packed.args).is_none(),
                    "{kind:?} must preserve direct execution for {}",
                    case.id
                );
            }
        }
    }
}
