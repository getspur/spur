use spur_tui::commands::{CommandEntry, CommandSource, Dispatch};

#[test]
fn command_entry_constructs() {
    let e = CommandEntry {
        name: "help".into(),
        description: "Show spur keybindings".into(),
        hint: None,
        source: CommandSource::Spur,
        dispatch: Dispatch::SpurLocal(spur_tui::action::Action::ShowHelp),
        arg_picker_spec: None,
    };
    assert_eq!(e.name, "help");
    assert!(matches!(e.source, CommandSource::Spur));
}

#[test]
fn command_source_agent_carries_handle() {
    let s = CommandSource::Agent {
        handle: "claude".into(),
    };
    match s {
        CommandSource::Agent { handle } => assert_eq!(handle, "claude"),
        _ => panic!("expected Agent"),
    }
}

#[test]
fn spur_local_source_exposes_v1_set() {
    let entries = spur_tui::commands::SpurLocalSource::entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"help"), "missing /help: {:?}", names);
    assert!(names.contains(&"mode"), "missing /mode: {:?}", names);
    assert!(names.contains(&"cost"), "missing /cost: {:?}", names);
    assert!(names.contains(&"quit"), "missing /quit: {:?}", names);
    assert!(names.contains(&"sprints"), "missing /sprints: {:?}", names);

    for e in &entries {
        assert!(matches!(e.source, spur_tui::commands::CommandSource::Spur));
        assert!(matches!(
            e.dispatch,
            spur_tui::commands::Dispatch::SpurLocal(_)
        ));
    }
}

use spur_acp::{AvailableCommand, AvailableCommandInput, UnstructuredCommandInput};
use spur_tui::commands::CommandRegistry;

fn acp_cmd(name: &str, desc: &str, hint: Option<&str>) -> AvailableCommand {
    let mut c = AvailableCommand::new(name, desc);
    if let Some(h) = hint {
        c = c.input(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new(h),
        ));
    }
    c
}

/// Helper: build a `CommandEntry` for an agent-advertised command using the
/// default prompt_text dispatch. Mirrors what `apply_session_update` does
/// for AvailableCommandsUpdate in production.
fn agent_entry(handle: &str, cmd: &AvailableCommand) -> CommandEntry {
    let cfg = spur_acp::CommandsConfig {
        dispatch: spur_acp::DispatchKind::PromptText,
        ..Default::default()
    };
    spur_tui::agents::build_entry(handle, &cfg, cmd)
}

#[test]
fn registry_merges_spur_local_and_agent() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![agent_entry("claude", &acp_cmd("compact", "compact", None))],
    );
    let entries = reg.list();

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"help"), "spur /help missing: {:?}", names);
    assert!(
        names.contains(&"compact"),
        "agent /compact missing: {:?}",
        names
    );
}

#[test]
fn registry_marks_collisions_with_source_prefix() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![agent_entry("claude", &acp_cmd("help", "claude help", None))],
    );

    let entries = reg.list();
    let helps: Vec<_> = entries.iter().filter(|e| e.name == "help").collect();
    assert_eq!(helps.len(), 2);

    let spur_help = helps
        .iter()
        .find(|e| e.source == CommandSource::Spur)
        .unwrap();
    let claude_help = helps
        .iter()
        .find(|e| matches!(&e.source, CommandSource::Agent { handle } if handle == "claude"))
        .unwrap();
    assert_eq!(reg.canonical_typed_form(spur_help), "/spur:help");
    assert_eq!(reg.canonical_typed_form(claude_help), "/claude:help");
}

#[test]
fn registry_unique_names_use_bare_form() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![agent_entry("claude", &acp_cmd("compact", "", None))],
    );
    let entries = reg.list();
    let compact = entries.iter().find(|e| e.name == "compact").unwrap();
    assert_eq!(reg.canonical_typed_form(compact), "/compact");
}

#[test]
fn registry_resolve_prefers_explicit_prefix() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![agent_entry("claude", &acp_cmd("help", "", None))],
    );
    let entry = reg.resolve("/claude:help").expect("match");
    assert!(matches!(&entry.source, CommandSource::Agent { handle } if handle == "claude"));
}

#[test]
fn registry_resolve_bare_ambiguous_prefers_spur() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![agent_entry("claude", &acp_cmd("help", "", None))],
    );
    let entry = reg.resolve("/help").expect("match");
    assert_eq!(entry.source, CommandSource::Spur);
}

#[test]
fn registry_resolve_unknown_returns_none() {
    let reg = CommandRegistry::new();
    assert!(reg.resolve("/does-not-exist").is_none());
    assert!(reg.resolve("hello world").is_none());
}

#[test]
fn fuzzy_rank_commands_prefers_prefix_matches() {
    use spur_tui::commands::fuzzy::rank;
    use spur_tui::commands::CommandRegistry;
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![
            agent_entry("claude", &acp_cmd("compact", "", None)),
            agent_entry("claude", &acp_cmd("config", "", None)),
            agent_entry("claude", &acp_cmd("doctor", "", None)),
        ],
    );
    let entries: Vec<_> = reg
        .list()
        .into_iter()
        .filter(|e| matches!(e.source, spur_tui::commands::CommandSource::Agent { .. }))
        .collect();
    let ranked = rank(&entries, "co");
    let names: Vec<&str> = ranked.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names[0] == "compact" || names[0] == "config",
        "top: {:?}",
        names
    );
    assert!(!names.contains(&"doctor") || names.iter().position(|n| *n == "doctor").unwrap() > 1);
}

#[test]
fn fuzzy_rank_empty_query_returns_input_order() {
    use spur_tui::commands::fuzzy::rank;
    use spur_tui::commands::CommandRegistry;
    let reg = CommandRegistry::new();
    let entries = reg.list();
    let ranked = rank(&entries, "");
    assert_eq!(ranked.len(), entries.len());
    assert_eq!(ranked[0].name, entries[0].name);
}
