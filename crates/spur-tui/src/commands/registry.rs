use spur_acp::{AvailableCommand, AvailableCommandInput};

use super::entry::{CommandEntry, CommandSource, Dispatch};
use super::spur_local::SpurLocalSource;

/// Merges spur-local and agent-advertised slash commands.
///
/// Collision rule: when a name is defined by more than one source, the
/// popup displays each variant separately. The *canonical typed form* of
/// an entry is bare (`/name`) when unique across sources, or prefixed
/// (`/<source>:<name>`) on collision. Resolution at submit time honors
/// explicit prefixes first, then falls back to spur-local-wins for
/// ambiguous bare names.
pub struct CommandRegistry {
    agent_commands: Vec<(String, Vec<AvailableCommand>)>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            agent_commands: Vec::new(),
        }
    }

    pub fn set_agent_commands(&mut self, handle: &str, cmds: Vec<AvailableCommand>) {
        if let Some(slot) = self
            .agent_commands
            .iter_mut()
            .find(|(h, _)| h == handle)
        {
            slot.1 = cmds;
        } else {
            self.agent_commands.push((handle.to_string(), cmds));
        }
    }

    pub fn list(&self) -> Vec<CommandEntry> {
        let mut out = SpurLocalSource::entries();
        for (handle, cmds) in &self.agent_commands {
            for c in cmds {
                out.push(agent_entry(handle, c));
            }
        }
        out
    }

    pub fn canonical_typed_form(&self, entry: &CommandEntry) -> String {
        let colliding = self.list().iter().filter(|e| e.name == entry.name).count() > 1;
        if colliding {
            match &entry.source {
                CommandSource::Spur => format!("/spur:{}", entry.name),
                CommandSource::Agent { handle } => format!("/{}:{}", handle, entry.name),
            }
        } else {
            format!("/{}", entry.name)
        }
    }

    pub fn resolve(&self, text: &str) -> Option<CommandEntry> {
        let rest = text.strip_prefix('/')?;
        let first_token = rest.split_whitespace().next()?;
        let entries = self.list();
        if let Some((source, name)) = first_token.split_once(':') {
            return entries.into_iter().find(|e| {
                e.name == name
                    && match (&e.source, source) {
                        (CommandSource::Spur, "spur") => true,
                        (CommandSource::Agent { handle }, s) => handle == s,
                        _ => false,
                    }
            });
        }
        let mut candidates: Vec<_> = entries
            .into_iter()
            .filter(|e| e.name == first_token)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|e| match &e.source {
            CommandSource::Spur => 0,
            CommandSource::Agent { .. } => 1,
        });
        candidates.into_iter().next()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn agent_entry(handle: &str, c: &AvailableCommand) -> CommandEntry {
    let hint = match &c.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };
    CommandEntry {
        name: c.name.clone(),
        description: c.description.clone(),
        hint,
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch: Dispatch::PromptText {
            normalized: format!("/{}", c.name),
        },
    }
}
