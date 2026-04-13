use std::cell::RefCell;
use std::collections::HashSet;

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
    /// Lazy merged view. Rebuilt only on `set_agent_commands`.
    cache: RefCell<Option<CacheSnapshot>>,
}

struct CacheSnapshot {
    entries: Vec<CommandEntry>,
    /// Names that appear more than once across sources — need prefix disambiguation.
    colliding: HashSet<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            agent_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    pub fn set_agent_commands(&mut self, handle: &str, cmds: Vec<AvailableCommand>) {
        if let Some(slot) = self.agent_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = cmds;
        } else {
            self.agent_commands.push((handle.to_string(), cmds));
        }
        *self.cache.borrow_mut() = None;
    }

    fn ensure_cache(&self) {
        let mut slot = self.cache.borrow_mut();
        if slot.is_some() {
            return;
        }
        let mut entries = SpurLocalSource::entries();
        for (handle, cmds) in &self.agent_commands {
            for c in cmds {
                entries.push(agent_entry(handle, c));
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut colliding: HashSet<String> = HashSet::new();
        for e in &entries {
            if !seen.insert(e.name.clone()) {
                colliding.insert(e.name.clone());
            }
        }
        *slot = Some(CacheSnapshot { entries, colliding });
    }

    pub fn list(&self) -> Vec<CommandEntry> {
        self.ensure_cache();
        self.cache.borrow().as_ref().unwrap().entries.clone()
    }

    pub fn canonical_typed_form(&self, entry: &CommandEntry) -> String {
        self.ensure_cache();
        let colliding = self
            .cache
            .borrow()
            .as_ref()
            .unwrap()
            .colliding
            .contains(&entry.name);
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
        self.ensure_cache();
        let cache = self.cache.borrow();
        let entries = &cache.as_ref().unwrap().entries;
        if let Some((source, name)) = first_token.split_once(':') {
            return entries.iter().find(|e| {
                e.name == name
                    && match (&e.source, source) {
                        (CommandSource::Spur, "spur") => true,
                        (CommandSource::Agent { handle }, s) => handle == s,
                        _ => false,
                    }
            }).cloned();
        }
        let mut candidates: Vec<_> = entries.iter().filter(|e| e.name == first_token).collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|e| match &e.source {
            CommandSource::Spur => 0,
            CommandSource::Agent { .. } => 1,
        });
        candidates.into_iter().next().cloned()
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
    let dispatch = if handle == "kiro" {
        Dispatch::KiroExecute {
            command: c.name.clone(),
            args: serde_json::json!({}),
        }
    } else {
        Dispatch::PromptText {
            normalized: format!("/{}", c.name),
        }
    };
    CommandEntry {
        name: c.name.clone(),
        description: c.description.clone(),
        hint,
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
    }
}
