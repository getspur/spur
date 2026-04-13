use super::entry::{CommandEntry, CommandSource, Dispatch};
use crate::action::Action;

/// Static registry of spur-local slash commands available in every session.
pub struct SpurLocalSource;

impl SpurLocalSource {
    pub fn entries() -> Vec<CommandEntry> {
        vec![
            CommandEntry {
                name: "help".into(),
                description: "Show spur keybindings".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowHelp),
            },
            CommandEntry {
                name: "mode".into(),
                description: "Toggle Claude session mode (plan / default)".into(),
                hint: Some("[plan|default]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::TogglePlanMode),
            },
            CommandEntry {
                name: "cost".into(),
                description: "Show current session cost".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowSessionCost),
            },
            CommandEntry {
                name: "quit".into(),
                description: "Quit spur".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::Quit),
            },
        ]
    }
}
