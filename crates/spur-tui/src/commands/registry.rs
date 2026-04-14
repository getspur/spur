use std::cell::RefCell;
use std::collections::HashSet;

use super::entry::{CommandEntry, CommandSource};
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
    agent_commands: Vec<(String, Vec<CommandEntry>)>,
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

    /// Replace the full command set for an agent handle. Entries are
    /// pre-built by the caller via `agents::build_entry`.
    pub fn set_agent_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
        if let Some(slot) = self.agent_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = entries;
        } else {
            self.agent_commands.push((handle.to_string(), entries));
        }
        *self.cache.borrow_mut() = None;
    }

    fn ensure_cache(&self) {
        let mut slot = self.cache.borrow_mut();
        if slot.is_some() {
            return;
        }
        let mut entries = SpurLocalSource::entries();
        for (_handle, agent_entries) in &self.agent_commands {
            entries.extend(agent_entries.iter().cloned());
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
