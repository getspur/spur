use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::entry::{CommandEntry, CommandSource, Dispatch};
use super::spur_local::SpurLocalSource;
use spur_acp::{AgentConfig, SpurAgentCaps};

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
    /// Per-agent commands synthesized by spur from advertised data
    /// (e.g. NewSessionResponse.config_options). Shadowed by spur-local
    /// exclusive meta-commands; otherwise visible alongside dynamic.
    advertised_commands: Vec<(String, Vec<CommandEntry>)>,
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
            advertised_commands: Vec::new(),
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
            advertised_commands: Vec::new(),
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

    /// Replace the full advertised (synthesized) command set for an agent
    /// handle. Entries are pre-built by the synthesizer in spur-acp from
    /// advertised session data such as `NewSessionResponse.config_options`.
    pub fn set_advertised_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
        if let Some(slot) = self
            .advertised_commands
            .iter_mut()
            .find(|(h, _)| h == handle)
        {
            slot.1 = entries;
        } else {
            self.advertised_commands.push((handle.to_string(), entries));
        }
        *self.cache.borrow_mut() = None;
    }

    /// Returns the parsed ArgPickerSpec for the named command, if it requires
    /// an arg picker. Used by TriggerDetector and InputCompletionPort.
    pub fn arg_picker_spec(
        &self,
        command_name: &str,
    ) -> Option<spur_acp::adapter::arg_picker_hint::ArgPickerSpec> {
        self.ensure_cache();
        let cache = self.cache.borrow();
        cache
            .as_ref()?
            .entries
            .iter()
            .find(|e| e.name == command_name)
            .and_then(|e| e.arg_picker_spec.clone())
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

        let spur_local_entries = SpurLocalSource::entries();

        // Meta-command precedence: spur-local entries that are "exclusive"
        // shadow any agent-advertised entry with the same name. See the
        // module comment in `spur_local.rs` for the taxonomy rationale.
        // Non-exclusive spur-local entries (e.g. /help) may still coexist
        // with agent entries of the same name (collision-display applies).
        let exclusive_names: HashSet<&str> =
            SpurLocalSource::exclusive_names().iter().copied().collect();

        let mut entries = spur_local_entries;

        // Static entries — include only if not overridden by a dynamic
        // entry at the same (handle, name), AND not shadowed by a
        // spur-local exclusive meta-command.
        for (handle, statics) in &self.static_commands {
            for s in statics {
                if exclusive_names.contains(s.name.as_str()) {
                    continue;
                }
                if !dynamic_index.contains_key(&(handle.as_str(), s.name.as_str())) {
                    entries.push(s.clone());
                }
            }
        }

        // Dynamic entries — include only if not shadowed by a spur-local
        // exclusive meta-command.
        for (_handle, dyn_entries) in &self.dynamic_commands {
            for e in dyn_entries {
                if exclusive_names.contains(e.name.as_str()) {
                    continue;
                }
                entries.push(e.clone());
            }
        }

        // Advertised entries (synthesized by spur from agent-advertised data
        // such as config_options). Same shadowing rule as dynamic: spur-local
        // exclusive meta-commands win.
        for (_handle, adv_entries) in &self.advertised_commands {
            for e in adv_entries {
                if exclusive_names.contains(e.name.as_str()) {
                    continue;
                }
                entries.push(e.clone());
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

    /// Caps-aware filter over `list()`. Spec §6.5 / Wave C.1.
    ///
    /// `caps == None` is **permissive** — every entry survives (F-3
    /// invariant: resumed sessions before M9 wires `LoadSessionResponse`
    /// must keep all pickers visible). When `caps` are present, entries
    /// whose `Dispatch` requires an unsupported capability are filtered:
    ///
    /// * `Dispatch::SetSessionConfigOption { config_id: "model" }`
    ///   ⇒ require `caps.supports_set_model()` *or*
    ///   `caps.supports_set_config_option()`.
    /// * `Dispatch::SetSessionConfigOption { config_id: _ }`
    ///   ⇒ require `caps.supports_set_config_option()`.
    /// * `Dispatch::SpurLocal`, `Dispatch::PromptText`, `Dispatch::VendorExec`
    ///   ⇒ always allowed.
    pub fn available_commands_for_session(
        &self,
        caps: Option<&SpurAgentCaps>,
    ) -> Vec<CommandEntry> {
        let entries = self.list();
        let Some(caps) = caps else {
            return entries;
        };
        entries
            .into_iter()
            .filter(|e| match &e.dispatch {
                Dispatch::SpurLocal(_)
                | Dispatch::PromptText { .. }
                | Dispatch::VendorExec { .. } => true,
                Dispatch::SetSessionConfigOption { config_id } => {
                    if config_id == "model" {
                        caps.supports_set_model() || caps.supports_set_config_option()
                    } else {
                        caps.supports_set_config_option()
                    }
                }
            })
            .collect()
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
                CommandSource::Advertised { handle } => format!("/{}:{}", handle, entry.name),
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
            return entries
                .iter()
                .find(|e| {
                    e.name == name
                        && match (&e.source, source) {
                            (CommandSource::Spur, "spur") => true,
                            (CommandSource::Agent { handle }, s) => handle == s,
                            (CommandSource::Advertised { handle }, s) => handle == s,
                            _ => false,
                        }
                })
                .cloned();
        }
        let mut candidates: Vec<_> = entries.iter().filter(|e| e.name == first_token).collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|e| match &e.source {
            CommandSource::Spur => 0,
            CommandSource::Agent { .. } => 1,
            CommandSource::Advertised { .. } => 1,
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
        assert!(registry
            .list()
            .iter()
            .all(|e| matches!(e.source, CommandSource::Spur)));
    }

    #[test]
    fn dynamic_overrides_static_on_same_handle_name() {
        let cfg = config_with_static("codex", "codex", vec!["compact"]);
        let mut registry = CommandRegistry::from_configs(&[cfg]);
        let dynamic = CommandEntry {
            name: "compact".into(),
            description: "DYNAMIC DESC".into(),
            hint: None,
            source: CommandSource::Agent {
                handle: "codex".into(),
            },
            dispatch: Dispatch::PromptText {
                normalized: "/compact".into(),
            },
            arg_picker_spec: None,
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(
            compacts.len(),
            1,
            "dynamic must replace static, not coexist"
        );
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
            source: CommandSource::Agent {
                handle: "codex".into(),
            },
            dispatch: Dispatch::PromptText {
                normalized: "/compact".into(),
            },
            arg_picker_spec: None,
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        registry.set_agent_commands("codex", vec![]);
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(compacts.len(), 1);
        assert_eq!(
            compacts[0].description, "compact desc",
            "static should reappear"
        );
    }

    #[test]
    fn cross_agent_same_name_still_disambiguates_with_prefix() {
        // Use a command name that is NOT a spur-local meta-command so
        // both agent entries survive into the registry and collide.
        let codex = config_with_static("codex", "codex", vec!["compact"]);
        let kiro = config_with_static("kiro", "kiro", vec!["compact"]);
        let registry = CommandRegistry::from_configs(&[codex, kiro]);
        let compact_entries: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact" && matches!(e.source, CommandSource::Agent { .. }))
            .collect();
        assert_eq!(compact_entries.len(), 2);
        let codex_compact = compact_entries
            .iter()
            .find(|e| matches!(&e.source, CommandSource::Agent { handle } if handle == "codex"))
            .unwrap();
        assert_eq!(
            registry.canonical_typed_form(codex_compact),
            "/codex:compact"
        );
    }

    #[test]
    fn spur_local_meta_command_shadows_agent_entry_with_same_name() {
        // Agent advertises /clear dynamically (kiro does this). The
        // spur-local /clear meta-command must take precedence and the
        // agent's /clear must NOT appear in the list.
        let mut registry = CommandRegistry::new();
        let agent_clear = CommandEntry {
            name: "clear".into(),
            description: "agent's own clear".into(),
            hint: None,
            source: CommandSource::Agent {
                handle: "kiro".into(),
            },
            dispatch: Dispatch::PromptText {
                normalized: "/clear".into(),
            },
            arg_picker_spec: None,
        };
        registry.set_agent_commands("kiro", vec![agent_clear]);

        let list = registry.list();
        let clear_entries: Vec<_> = list.iter().filter(|e| e.name == "clear").collect();
        assert_eq!(
            clear_entries.len(),
            1,
            "agent /clear must be shadowed by spur-local /clear"
        );
        assert!(
            matches!(clear_entries[0].source, CommandSource::Spur),
            "the surviving /clear must be spur-local"
        );
    }

    #[test]
    fn advertised_commands_appear_in_cache() {
        let mut reg = CommandRegistry::new();
        let entry = CommandEntry {
            name: "model".into(),
            description: "Switch model".into(),
            hint: None,
            source: CommandSource::Advertised {
                handle: "codex".into(),
            },
            dispatch: Dispatch::SetSessionConfigOption {
                config_id: "model".into(),
            },
            arg_picker_spec: None,
        };
        reg.set_advertised_commands("codex", vec![entry]);
        let names: Vec<_> = reg.list().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"model".to_string()));
    }

    #[test]
    fn spur_local_shadows_advertised_with_same_name() {
        // spur-local /clear is an exclusive meta-command (per SpurLocalSource).
        // Verify an advertised /clear from an agent is shadowed.
        let mut reg = CommandRegistry::new();
        let advertised_clear = CommandEntry {
            name: "clear".into(),
            description: "agent's clear".into(),
            hint: None,
            source: CommandSource::Advertised {
                handle: "codex".into(),
            },
            dispatch: Dispatch::SetSessionConfigOption {
                config_id: "clear".into(),
            },
            arg_picker_spec: None,
        };
        reg.set_advertised_commands("codex", vec![advertised_clear]);
        let clear_entries: Vec<_> = reg
            .list()
            .into_iter()
            .filter(|e| e.name == "clear")
            .collect();
        assert_eq!(
            clear_entries.len(),
            1,
            "advertised /clear must be shadowed by spur-local /clear"
        );
        assert!(matches!(clear_entries[0].source, CommandSource::Spur));
    }

    /// Wave C.1: caps = None ⇒ permissive — every entry survives the
    /// filter. This is the F-3 invariant: resumed sessions before M9
    /// wires `LoadSessionResponse` are caps=None and must keep all
    /// pickers visible (no regression).
    #[test]
    fn available_commands_for_session_with_none_caps_returns_all_entries() {
        let mut reg = CommandRegistry::new();
        reg.set_advertised_commands(
            "codex",
            vec![
                CommandEntry {
                    name: "model".into(),
                    description: "Switch model".into(),
                    hint: None,
                    source: CommandSource::Advertised {
                        handle: "codex".into(),
                    },
                    dispatch: Dispatch::SetSessionConfigOption {
                        config_id: "model".into(),
                    },
                    arg_picker_spec: None,
                },
                CommandEntry {
                    name: "effort".into(),
                    description: "Switch effort".into(),
                    hint: None,
                    source: CommandSource::Advertised {
                        handle: "codex".into(),
                    },
                    dispatch: Dispatch::SetSessionConfigOption {
                        config_id: "effort".into(),
                    },
                    arg_picker_spec: None,
                },
            ],
        );
        let names: Vec<_> = reg
            .available_commands_for_session(None)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"model".to_string()));
        assert!(names.contains(&"effort".to_string()));
    }

    /// Wave C.2: codex caps (all set_* gates true) ⇒ all entries survive.
    #[test]
    fn available_commands_for_session_with_codex_caps_keeps_all_entries() {
        use agent_client_protocol::schema::{
            InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion,
            SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigSelect,
            SessionConfigSelectOptions, SessionConfigValueId, SessionId, SessionModelState,
        };

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new =
            NewSessionResponse::new(SessionId::new("sid")).models(SessionModelState::new(
                ModelId::new("gpt-5-codex"),
                vec![ModelInfo::new(ModelId::new("gpt-5-codex"), "GPT-5 Codex")],
            ));
        new.config_options = Some(vec![SessionConfigOption::new(
            SessionConfigId::new("model"),
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new("gpt-5-codex"),
                SessionConfigSelectOptions::Ungrouped(vec![]),
            )),
        )]);
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::CodexAcp);
        assert!(caps.supports_set_model());
        assert!(caps.supports_set_config_option());

        let mut reg = CommandRegistry::new();
        reg.set_advertised_commands(
            "codex",
            vec![CommandEntry {
                name: "model".into(),
                description: "Switch model".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "codex".into(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: "model".into(),
                },
                arg_picker_spec: None,
            }],
        );
        let names: Vec<_> = reg
            .available_commands_for_session(Some(&caps))
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"model".to_string()));
    }

    /// Wave C.2: gemini-style caps (no set_config_option, no set_model)
    /// ⇒ /model and /effort are filtered out of the popup.
    #[test]
    fn available_commands_for_session_with_gemini_style_caps_hides_config_option_pickers() {
        use agent_client_protocol::schema::{
            InitializeResponse, NewSessionResponse, ProtocolVersion, SessionId,
        };

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        // Gemini-style: no models, no config_options. set_model & set_config_option both false.
        let new = NewSessionResponse::new(SessionId::new("sid"));
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::Generic);
        assert!(!caps.supports_set_model());
        assert!(!caps.supports_set_config_option());

        let mut reg = CommandRegistry::new();
        reg.set_advertised_commands(
            "gemini",
            vec![
                CommandEntry {
                    name: "model".into(),
                    description: "Switch model".into(),
                    hint: None,
                    source: CommandSource::Advertised {
                        handle: "gemini".into(),
                    },
                    dispatch: Dispatch::SetSessionConfigOption {
                        config_id: "model".into(),
                    },
                    arg_picker_spec: None,
                },
                CommandEntry {
                    name: "effort".into(),
                    description: "Switch effort".into(),
                    hint: None,
                    source: CommandSource::Advertised {
                        handle: "gemini".into(),
                    },
                    dispatch: Dispatch::SetSessionConfigOption {
                        config_id: "effort".into(),
                    },
                    arg_picker_spec: None,
                },
            ],
        );
        let names: Vec<_> = reg
            .available_commands_for_session(Some(&caps))
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            !names.contains(&"model".to_string()),
            "/model must be hidden when caps lack both set_model and set_config_option"
        );
        assert!(
            !names.contains(&"effort".to_string()),
            "/effort must be hidden when caps lack set_config_option"
        );
    }

    /// Wave C.2: caps with only set_model (gemini-style + models populated)
    /// keeps /model visible — supports_set_model() is enough.
    #[test]
    fn available_commands_for_session_with_models_only_keeps_model_picker() {
        use agent_client_protocol::schema::{
            InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion, SessionId,
            SessionModelState,
        };

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let new = NewSessionResponse::new(SessionId::new("sid")).models(SessionModelState::new(
            ModelId::new("gemini-1.5-pro"),
            vec![ModelInfo::new(
                ModelId::new("gemini-1.5-pro"),
                "Gemini 1.5 Pro",
            )],
        ));
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::Generic);
        assert!(caps.supports_set_model());
        assert!(!caps.supports_set_config_option());

        let mut reg = CommandRegistry::new();
        reg.set_advertised_commands(
            "gemini",
            vec![CommandEntry {
                name: "model".into(),
                description: "Switch model".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "gemini".into(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: "model".into(),
                },
                arg_picker_spec: None,
            }],
        );
        let names: Vec<_> = reg
            .available_commands_for_session(Some(&caps))
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            names.contains(&"model".to_string()),
            "/model must remain when caps support set_model (even without set_config_option)"
        );
    }

    /// Wave C.1: spur-local + agent PromptText entries always survive
    /// the filter regardless of caps state.
    #[test]
    fn available_commands_for_session_keeps_prompt_text_and_spur_local() {
        use agent_client_protocol::schema::{
            InitializeResponse, NewSessionResponse, ProtocolVersion, SessionId,
        };

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let caps = spur_acp::SpurAgentCaps::new(
            &init,
            &NewSessionResponse::new(SessionId::new("sid")),
            spur_acp::AgentKind::Generic,
        );

        let mut reg = CommandRegistry::new();
        reg.set_agent_commands(
            "kiro",
            vec![CommandEntry {
                name: "context".into(),
                description: "show context".into(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "kiro".into(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/context".into(),
                },
                arg_picker_spec: None,
            }],
        );
        let names: Vec<_> = reg
            .available_commands_for_session(Some(&caps))
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            names.contains(&"context".to_string()),
            "PromptText commands must always pass through the filter"
        );
        // spur-local /clear is exclusive — it should always be present too.
        assert!(
            names.iter().any(|n| n == "clear"),
            "spur-local meta-commands must always survive caps filtering"
        );
    }

    #[test]
    fn arg_picker_spec_returns_some_for_advertised_with_spec() {
        let mut reg = CommandRegistry::new();
        let entry = CommandEntry {
            name: "model".into(),
            description: "Switch".into(),
            hint: None,
            source: CommandSource::Advertised {
                handle: "codex".into(),
            },
            dispatch: Dispatch::SetSessionConfigOption {
                config_id: "model".into(),
            },
            arg_picker_spec: Some(spur_acp::adapter::arg_picker_hint::ArgPickerSpec {
                free_text_hint: String::new(),
                typed_hint: Some(
                    spur_acp::adapter::arg_picker_hint::ArgPickerHint::ConfigOption {
                        config_id: "model".into(),
                    },
                ),
            }),
        };
        reg.set_advertised_commands("codex", vec![entry]);
        assert!(reg.arg_picker_spec("model").is_some());
        assert!(reg.arg_picker_spec("nonexistent").is_none());
    }
}
