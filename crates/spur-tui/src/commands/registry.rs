use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::entry::{CommandEntry, CommandSource};
use super::spur_local::SpurLocalSource;
use spur_acp::AgentConfig;

/// Merges spur-local, static (config), and dynamic (runtime) slash
/// commands.
///
/// Collision rules:
/// * Same `(handle, name)` across static + dynamic → dynamic wins.
/// * Different handles with the same `name` → popup shows both; resolver
///   uses prefix disambiguation via `canonical_typed_form`.
pub struct CommandRegistry {
    /// Per-agent static commands from `[[commands.static]]`.
    static_commands: Vec<(String, Vec<CommandEntry>)>,
    /// Per-agent commands received via ingest at runtime.
    dynamic_commands: Vec<(String, Vec<CommandEntry>)>,
    /// Lazy merged view. Rebuilt on any mutation.
    cache: RefCell<Option<CacheSnapshot>>,
}

struct CacheSnapshot {
    entries: Vec<CommandEntry>,
    colliding: HashSet<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            static_commands: Vec::new(),
            dynamic_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    /// Build a registry pre-populated with static commands from `configs`.
    /// Static commands become visible in the popup before any agent
    /// connects; dynamic commands received later override these on
    /// `(handle, name)` match.
    pub fn from_configs(configs: &[AgentConfig]) -> Self {
        let static_commands = configs
            .iter()
            .filter(|c| !c.commands.static_commands.is_empty())
            .map(|c| {
                let handle = c.effective_handle();
                let entries = c
                    .commands
                    .static_commands
                    .iter()
                    .map(|decl| crate::agents::build_static_entry(&handle, &c.commands, decl))
                    .collect();
                (handle, entries)
            })
            .collect();
        Self {
            static_commands,
            dynamic_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    /// Replace the full dynamic command set for an agent handle. Entries
    /// are pre-built by the caller via `agents::build_entry`.
    pub fn set_agent_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
        if let Some(slot) = self.dynamic_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = entries;
        } else {
            self.dynamic_commands.push((handle.to_string(), entries));
        }
        *self.cache.borrow_mut() = None;
    }

    fn ensure_cache(&self) {
        let mut slot = self.cache.borrow_mut();
        if slot.is_some() {
            return;
        }

        // Build (handle, name) → dynamic-entry index for O(1) override lookup.
        let mut dynamic_index: HashMap<(&str, &str), &CommandEntry> = HashMap::new();
        for (handle, entries) in &self.dynamic_commands {
            for e in entries {
                dynamic_index.insert((handle.as_str(), e.name.as_str()), e);
            }
        }

        let mut entries = SpurLocalSource::entries();

        // Static entries — include only if not overridden by a dynamic entry
        // at the same (handle, name).
        for (handle, statics) in &self.static_commands {
            for s in statics {
                if !dynamic_index.contains_key(&(handle.as_str(), s.name.as_str())) {
                    entries.push(s.clone());
                }
            }
        }

        // Dynamic entries — always included.
        for (_handle, dyn_entries) in &self.dynamic_commands {
            entries.extend(dyn_entries.iter().cloned());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
    use spur_acp::{AgentConfig, CommandsConfig, DispatchKind, StaticCommandDecl};

    fn config_with_static(name: &str, handle: &str, statics: Vec<&str>) -> AgentConfig {
        let mut cfg = AgentConfig::with_defaults(name);
        cfg.display.handle = Some(handle.to_string());
        cfg.commands = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            static_commands: statics
                .into_iter()
                .map(|n| StaticCommandDecl {
                    name: n.into(),
                    description: format!("{n} desc"),
                    hint: None,
                })
                .collect(),
            ..Default::default()
        };
        cfg
    }

    #[test]
    fn from_configs_loads_static_commands_at_construction() {
        let cfg = config_with_static("codex", "codex", vec!["compact", "model"]);
        let registry = CommandRegistry::from_configs(&[cfg]);
        let names: Vec<_> = registry.list().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"compact".to_string()));
        assert!(names.contains(&"model".to_string()));
    }

    #[test]
    fn from_configs_without_statics_is_empty() {
        let cfg = AgentConfig::with_defaults("codex");
        let registry = CommandRegistry::from_configs(&[cfg]);
        // Only spur-local commands present; no agent commands.
        assert!(registry.list().iter().all(|e| matches!(e.source, CommandSource::Spur)));
    }

    #[test]
    fn dynamic_overrides_static_on_same_handle_name() {
        let cfg = config_with_static("codex", "codex", vec!["compact"]);
        let mut registry = CommandRegistry::from_configs(&[cfg]);
        let dynamic = CommandEntry {
            name: "compact".into(),
            description: "DYNAMIC DESC".into(),
            hint: None,
            source: CommandSource::Agent { handle: "codex".into() },
            dispatch: Dispatch::PromptText { normalized: "/compact".into() },
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(compacts.len(), 1, "dynamic must replace static, not coexist");
        assert_eq!(compacts[0].description, "DYNAMIC DESC");
    }

    #[test]
    fn clearing_dynamic_reveals_static_again() {
        let cfg = config_with_static("codex", "codex", vec!["compact"]);
        let mut registry = CommandRegistry::from_configs(&[cfg]);
        let dynamic = CommandEntry {
            name: "compact".into(),
            description: "DYNAMIC".into(),
            hint: None,
            source: CommandSource::Agent { handle: "codex".into() },
            dispatch: Dispatch::PromptText { normalized: "/compact".into() },
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        registry.set_agent_commands("codex", vec![]);
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(compacts.len(), 1);
        assert_eq!(compacts[0].description, "compact desc", "static should reappear");
    }

    #[test]
    fn cross_agent_same_name_still_disambiguates_with_prefix() {
        let codex = config_with_static("codex", "codex", vec!["help"]);
        let kiro = config_with_static("kiro", "kiro", vec!["help"]);
        let registry = CommandRegistry::from_configs(&[codex, kiro]);
        // Filter to agent-sourced entries only; spur-local also has /help.
        let help_entries: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "help" && matches!(e.source, CommandSource::Agent { .. }))
            .collect();
        assert_eq!(help_entries.len(), 2);
        let codex_help = help_entries
            .iter()
            .find(|e| matches!(&e.source, CommandSource::Agent { handle } if handle == "codex"))
            .unwrap();
        assert_eq!(registry.canonical_typed_form(codex_help), "/codex:help");
    }
}
