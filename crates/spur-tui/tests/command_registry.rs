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

#[test]
fn spur_local_source_exposes_v1_set() {
    let entries = spur_tui::commands::SpurLocalSource::entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"help"), "missing /help: {:?}", names);
    assert!(names.contains(&"mode"), "missing /mode: {:?}", names);
    assert!(names.contains(&"cost"), "missing /cost: {:?}", names);
    assert!(names.contains(&"quit"), "missing /quit: {:?}", names);

    for e in &entries {
        assert!(matches!(e.source, spur_tui::commands::CommandSource::Spur));
        assert!(matches!(e.dispatch, spur_tui::commands::Dispatch::SpurLocal(_)));
    }
}
