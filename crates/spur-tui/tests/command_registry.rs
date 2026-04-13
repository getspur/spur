use spur_tui::commands::{CommandEntry, CommandSource, Dispatch};

#[test]
fn command_entry_constructs() {
    let e = CommandEntry {
        name: "help".into(),
        description: "Show spur keybindings".into(),
        hint: None,
        source: CommandSource::Spur,
        dispatch: Dispatch::SpurLocal(spur_tui::action::Action::ShowHelp),
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
