//! Synthesizes CommandEntry rows from an agent's cached config_options.
//! Vendor-neutral; calls into spur-acp's config-option synthesizers.

use spur_acp::adapter::arg_picker_hint::{ArgPickerChoice, ArgPickerHint, ArgPickerSpec};
use spur_acp::adapter::config_options::{synthesize, synthesize_advertised, AdvertisedCommand};
use spur_acp::{SessionConfigOption, SpurAgentCaps};

use super::entry::{CommandEntry, CommandSource, Dispatch};

pub struct AdvertisedSource;

impl AdvertisedSource {
    /// Build `CommandEntry` rows from a per-session capability snapshot.
    pub fn entries_from_caps(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
        let mut entries = synthesize_advertised(caps)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_string(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect::<Vec<_>>();
        entries.extend(mode_entries(handle, caps));
        entries.extend(grok_entries(handle, caps));
        entries.extend(kiro_entries(handle, caps));
        entries
    }

    /// Build CommandEntry rows from cached config_options. Each entry's
    /// `arg_picker_spec` is set from the synthesizer output.
    pub fn entries(handle: &str, opts: &[SessionConfigOption]) -> Vec<CommandEntry> {
        synthesize(opts)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_string(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect()
    }
}

fn mode_entries(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
    let Some(modes) = caps.modes.as_ref().filter(|_| caps.supports_set_mode()) else {
        return Vec::new();
    };
    vec![CommandEntry {
        name: "mode".to_string(),
        description: "Switch agent session mode".to_string(),
        hint: Some("[mode]".to_string()),
        source: CommandSource::Advertised {
            handle: handle.to_string(),
        },
        dispatch: Dispatch::SetSessionMode,
        arg_picker_spec: Some(ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::StaticChoices {
                choices: modes
                    .available_modes
                    .iter()
                    .map(|mode| ArgPickerChoice {
                        value: mode.id.0.to_string(),
                        label: mode.name.clone(),
                        description: None,
                    })
                    .collect(),
            }),
        }),
    }]
}

fn grok_entries(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
    if !caps.supports_grok_set_model() {
        return Vec::new();
    }
    let Some(display) = caps.grok_display.as_ref() else {
        return Vec::new();
    };
    let source = || CommandSource::Advertised {
        handle: handle.to_string(),
    };
    let picker = |choices: Vec<ArgPickerChoice>| {
        Some(ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::StaticChoices { choices }),
        })
    };
    let mut entries = vec![CommandEntry {
        name: "model".to_string(),
        description: "Switch model for this session".to_string(),
        hint: display.model_label.clone(),
        source: source(),
        dispatch: Dispatch::SetSessionModel,
        arg_picker_spec: picker(
            display
                .models()
                .iter()
                .map(|model| ArgPickerChoice {
                    value: model.id.clone(),
                    label: model.label.clone(),
                    description: None,
                })
                .collect(),
        ),
    }];

    let effort_choices = display
        .model_id
        .as_deref()
        .map(|model_id| display.efforts_for_model(model_id))
        .unwrap_or_default();
    if !effort_choices.is_empty() {
        entries.push(CommandEntry {
            name: "effort".to_string(),
            description: "Switch reasoning / thinking effort".to_string(),
            hint: display.effort_label.clone(),
            source: source(),
            dispatch: Dispatch::SetSessionEffort,
            arg_picker_spec: picker(
                effort_choices
                    .iter()
                    .map(|effort| ArgPickerChoice {
                        value: effort.id.clone(),
                        label: effort.label.clone(),
                        description: None,
                    })
                    .collect(),
            ),
        });
    }
    entries
}

/// Kiro `/model` from the recovered models plane (no effort surface).
fn kiro_entries(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
    if !caps.supports_kiro_set_model() {
        return Vec::new();
    }
    let Some(display) = caps.kiro_display.as_ref() else {
        return Vec::new();
    };
    // Avoid double-emitting /model when configOptions already synthesized one
    // (future Kiro builds that advertise a model select).
    if caps.supports_set_model() {
        return Vec::new();
    }
    vec![CommandEntry {
        name: "model".to_string(),
        description: "Switch model for this session".to_string(),
        hint: display.model_label.clone(),
        source: CommandSource::Advertised {
            handle: handle.to_string(),
        },
        dispatch: Dispatch::SetSessionModel,
        arg_picker_spec: Some(ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::StaticChoices {
                choices: display
                    .models()
                    .iter()
                    .map(|model| ArgPickerChoice {
                        value: model.id.clone(),
                        label: model.label.clone(),
                        description: model.description.clone(),
                    })
                    .collect(),
            }),
        }),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{
        AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
        SessionConfigOption, SessionConfigSelectOption, SessionMode, SessionModeId,
        SessionModeState, SpurAgentCaps,
    };

    fn caps_with_modes() -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.modes = Some(SessionModeState::new(
            SessionModeId::new("read-only"),
            vec![
                SessionMode::new(SessionModeId::new("read-only"), "Ask for approval"),
                SessionMode::new(SessionModeId::new("agent"), "Agent"),
                SessionMode::new(
                    SessionModeId::new("agent-full-access"),
                    "Agent (full access)",
                ),
            ],
        ));
        SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp)
    }

    #[test]
    fn empty_options_yield_empty_entries() {
        assert!(AdvertisedSource::entries("codex", &[]).is_empty());
    }

    #[test]
    fn allowlisted_option_yields_advertised_entry() {
        let opt = SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "label".to_string(),
            "gpt-5-codex".to_string(),
            vec![SessionConfigSelectOption::new(
                "gpt-5-codex".to_string(),
                "GPT-5 Codex".to_string(),
            )],
        );
        let entries = AdvertisedSource::entries("codex", &[opt]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
        assert!(matches!(
            entries[0].source,
            CommandSource::Advertised { ref handle } if handle == "codex"
        ));
        assert!(matches!(
            entries[0].dispatch,
            Dispatch::SetSessionConfigOption { ref config_id } if config_id == "model"
        ));
        assert!(entries[0].arg_picker_spec.is_some());
    }

    #[test]
    fn model_caps_yield_model_advertised_entry() {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gemini-3.1-pro-preview",
            vec![SessionConfigSelectOption::new(
                "gemini-3.1-pro-preview",
                "Gemini 3.1 Pro Preview",
            )],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Gemini);
        let entries = AdvertisedSource::entries_from_caps("gemini", &caps);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
    }

    #[test]
    fn agent_modes_yield_mode_entry_with_advertised_ids_and_labels() {
        let entries = AdvertisedSource::entries_from_caps("codex", &caps_with_modes());
        let mode = entries
            .iter()
            .find(|entry| entry.name == "mode")
            .expect("advertised modes must synthesize /mode");
        let spec = mode
            .arg_picker_spec
            .as_ref()
            .expect("/mode must expose the agent mode catalog");
        let Some(ArgPickerHint::StaticChoices { choices }) = spec.typed_hint.as_ref() else {
            panic!("/mode must use static advertised choices");
        };
        assert_eq!(
            choices
                .iter()
                .map(|choice| (choice.value.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("read-only", "Ask for approval"),
                ("agent", "Agent"),
                ("agent-full-access", "Agent (full access)"),
            ]
        );
    }

    fn grok_caps() -> SpurAgentCaps {
        let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
        init.meta = Some(
            serde_json::json!({
                "modelState": {
                    "currentModelId": "grok-4.5",
                    "availableModels": [
                        {
                            "modelId": "grok-4.5",
                            "name": "Grok 4.5",
                            "_meta": {
                                "reasoningEffort": "high",
                                "reasoningEfforts": [
                                    {"id": "high", "label": "High Effort"},
                                    {"id": "low", "label": "Low Effort"}
                                ]
                            }
                        },
                        {
                            "modelId": "grok-composer-2.5-fast",
                            "name": "Grok Composer 2.5 Fast",
                            "_meta": {"reasoningEfforts": []}
                        }
                    ]
                }
            })
            .as_object()
            .expect("meta fixture must be an object")
            .clone(),
        );
        SpurAgentCaps::new(
            &init,
            &NewSessionResponse::new(spur_acp::AcpSessionId::new("sid")),
            AgentKind::Grok,
        )
    }

    #[test]
    fn grok_catalog_yields_dedicated_model_and_effort_entries() {
        let caps = grok_caps();
        assert!(!caps.supports_set_config_option());

        let entries = AdvertisedSource::entries_from_caps("grok", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "effort"]
        );
        assert!(matches!(entries[0].dispatch, Dispatch::SetSessionModel));
        assert!(matches!(entries[1].dispatch, Dispatch::SetSessionEffort));
        let effort_spec = entries[1]
            .arg_picker_spec
            .as_ref()
            .expect("effort command must have a picker");
        assert!(matches!(
            effort_spec.typed_hint.as_ref(),
            Some(spur_acp::adapter::arg_picker_hint::ArgPickerHint::StaticChoices { choices })
                if choices.iter().map(|choice| choice.value.as_str()).collect::<Vec<_>>()
                    == vec!["high", "low"]
        ));

        let mut registry = crate::commands::CommandRegistry::new();
        registry.set_advertised_commands("grok", entries);
        let visible = registry.available_commands_for_session(Some(&caps));
        assert!(visible.iter().any(|entry| entry.name == "model"));
        assert!(visible.iter().any(|entry| entry.name == "effort"));
    }

    #[test]
    fn grok_composer_model_hides_effort_entry_after_notification() {
        let mut caps = grok_caps();
        assert!(caps.apply_grok_model_changed(&serde_json::json!({
            "sessionId": "sid",
            "update": {
                "sessionUpdate": "model_changed",
                "model_id": "grok-composer-2.5-fast"
            }
        })));

        let entries = AdvertisedSource::entries_from_caps("grok", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model"]
        );
    }

    fn kiro_caps() -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.meta = Some(
            serde_json::json!({
                "spur.recoveredModels": {
                    "availableModels": [
                        {"modelId": "auto", "name": "auto", "description": "task-picked"},
                        {
                            "modelId": "claude-sonnet-4.5",
                            "name": "claude-sonnet-4.5",
                            "description": "Claude Sonnet 4.5 model"
                        }
                    ],
                    "currentModelId": "claude-sonnet-4.5"
                }
            })
            .as_object()
            .expect("meta fixture must be an object")
            .clone(),
        );
        SpurAgentCaps::new(&init, &new, AgentKind::Kiro)
    }

    #[test]
    fn kiro_recovered_catalog_yields_dedicated_model_entry() {
        let caps = kiro_caps();
        assert!(!caps.supports_set_config_option());
        assert!(caps.supports_kiro_set_model());

        let entries = AdvertisedSource::entries_from_caps("kiro", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model"]
        );
        assert!(matches!(entries[0].dispatch, Dispatch::SetSessionModel));
        let model_spec = entries[0]
            .arg_picker_spec
            .as_ref()
            .expect("model command must have a picker");
        assert!(matches!(
            model_spec.typed_hint.as_ref(),
            Some(spur_acp::adapter::arg_picker_hint::ArgPickerHint::StaticChoices { choices })
                if choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>()
                    == vec!["auto", "claude-sonnet-4.5"]
        ));

        let mut registry = crate::commands::CommandRegistry::new();
        registry.set_advertised_commands("kiro", entries);
        let visible = registry.available_commands_for_session(Some(&caps));
        assert!(visible.iter().any(|entry| entry.name == "model"));
    }
}
