//! Synthesizes interactive `AdvertisedCommand` rows from the agent's
//! `Vec<SessionConfigOption>`. Vendor-neutral by `config_id` allow-list.

use agent_client_protocol::schema::{
    SessionConfigKind, SessionConfigOption, SessionConfigSelectOptions,
};

use super::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};
use crate::SpurAgentCaps;

/// Vendor-neutral description of an interactive slash command synthesized from
/// the agent's advertised config options. spur-tui consumes this without
/// needing ACP schema imports.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedCommand {
    /// Slash name (no leading `/`). E.g. "model", "effort".
    pub name: String,
    /// Short label for the slash popup.
    pub description: String,
    /// Optional hint, e.g. the current value.
    pub hint: Option<String>,
    /// ACP `config_id` to send back in `set_config_option`. May differ from
    /// `name` (we rename `reasoning_effort` → `effort` at the slash surface).
    pub config_id: String,
    /// Currently-selected value, if any.
    pub current_value: Option<String>,
    /// Selectable choices, in the order advertised by the agent.
    pub choices: Vec<AdvertisedChoice>,
    /// Picker descriptor consumed by spur-tui.
    pub arg_picker_spec: ArgPickerSpec,
}

/// One option in a synthesized command's picker.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedChoice {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Vendor-neutral allow-list. Tuple is `(acp_config_id, slash_name, slash_desc)`.
/// `slash_name` may differ from `acp_config_id` (e.g. `reasoning_effort` →
/// `effort` at the slash surface).
const ALLOW_LIST: &[(&str, &str, &str)] = &[
    ("model", "model", "Switch model for this session"),
    (
        "reasoning_effort",
        "effort",
        "Switch reasoning / thinking effort",
    ),
];

/// Extract the selectable choices from a `SessionConfigOption`'s payload, in
/// advertised order. Returns an empty `Vec` for non-Select kinds and for
/// Grouped select payloads (v1 only handles flat lists). Used by the TUI to
/// instantiate `ConfigOptionQuerySource` from cached options.
pub fn extract_choices(opt: &SessionConfigOption) -> Vec<AdvertisedChoice> {
    match &opt.kind {
        SessionConfigKind::Select(select) => match &select.options {
            SessionConfigSelectOptions::Ungrouped(choices) => choices
                .iter()
                .map(|c| AdvertisedChoice {
                    value: c.value.0.to_string(),
                    label: c.name.clone(),
                    description: c.description.clone(),
                })
                .collect(),
            // Grouped lists are a future-spec feature — v1 does not flatten.
            _ => Vec::new(),
        },
        // Future kinds (Boolean, etc.) → no select choices.
        _ => Vec::new(),
    }
}

pub fn synthesize(options: &[SessionConfigOption]) -> Vec<AdvertisedCommand> {
    let mut out = Vec::new();
    for (acp_config_id, slash_name, slash_desc) in ALLOW_LIST {
        let Some(opt) = options.iter().find(|o| o.id.0.as_ref() == *acp_config_id) else {
            continue;
        };

        let SessionConfigKind::Select(select) = &opt.kind else {
            continue;
        };

        // v1 only handles flat (ungrouped) option lists. Grouped lists are a
        // future-spec feature — skip the command rather than guess.
        let SessionConfigSelectOptions::Ungrouped(choices_acp) = &select.options else {
            continue;
        };

        if choices_acp.is_empty() {
            continue;
        }

        let choices: Vec<AdvertisedChoice> = choices_acp
            .iter()
            .map(|c| AdvertisedChoice {
                value: c.value.0.to_string(),
                label: c.name.clone(),
                description: c.description.clone(),
            })
            .collect();

        let current = select.current_value.0.as_ref();
        let current_value = if current.is_empty() {
            None
        } else {
            Some(current.to_string())
        };
        let hint = current_value.as_ref().map(|v| format!("current: {v}"));

        out.push(AdvertisedCommand {
            name: (*slash_name).to_string(),
            description: (*slash_desc).to_string(),
            hint,
            config_id: (*acp_config_id).to_string(),
            current_value,
            choices,
            arg_picker_spec: ArgPickerSpec {
                free_text_hint: String::new(),
                typed_hint: Some(ArgPickerHint::ConfigOption {
                    config_id: (*acp_config_id).to_string(),
                }),
            },
        });
    }
    out
}

/// Synthesize advertised slash commands from frozen session capabilities.
///
/// Precedence:
/// 1) Use allow-listed `config_options` exactly like [`synthesize`].
/// 2) If no `model` command was emitted and `models.available_models` is
///    non-empty, synthesize a `model` command from `SessionModelState`.
pub fn synthesize_advertised(caps: &SpurAgentCaps) -> Vec<AdvertisedCommand> {
    let mut out = synthesize(&caps.config_options);

    let has_model_command = out.iter().any(|cmd| cmd.config_id == "model");
    if has_model_command {
        return out;
    }

    let Some(models) = caps
        .models
        .as_ref()
        .filter(|models| !models.available_models.is_empty())
    else {
        return out;
    };

    let current_model_id = models.current_model_id.0.to_string();
    out.push(AdvertisedCommand {
        name: "model".to_string(),
        description: "Switch model for this session".to_string(),
        hint: Some(
            caps.current_model_label()
                .unwrap_or_else(|| current_model_id.clone()),
        ),
        config_id: "model".to_string(),
        current_value: Some(current_model_id),
        choices: models
            .available_models
            .iter()
            .map(|model| AdvertisedChoice {
                value: model.model_id.0.to_string(),
                label: model.name.clone(),
                description: model.description.clone(),
            })
            .collect(),
        arg_picker_spec: ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::ConfigOption {
                config_id: "model".to_string(),
            }),
        },
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentKind;
    use agent_client_protocol::schema::{
        InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion,
        SessionConfigOption, SessionConfigSelectOption, SessionId, SessionModelState,
    };

    fn make_select(
        config_id: &str,
        current: &str,
        choices: &[(&str, &str)],
    ) -> SessionConfigOption {
        let select_choices: Vec<SessionConfigSelectOption> = choices
            .iter()
            .map(|(id, name)| {
                SessionConfigSelectOption::new((*id).to_string(), (*name).to_string())
            })
            .collect();
        SessionConfigOption::select(
            config_id.to_string(),
            "label".to_string(),
            current.to_string(),
            select_choices,
        )
    }

    fn make_caps(
        config_options: Vec<SessionConfigOption>,
        models: Option<SessionModelState>,
    ) -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(SessionId::new("test-caps"));
        new.config_options = Some(config_options);
        if let Some(models) = models {
            new = new.models(models);
        }
        SpurAgentCaps::new(&init, &new, AgentKind::Generic)
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(synthesize(&[]).is_empty());
    }

    #[test]
    fn single_allowlisted_select_emits_one_command_with_ordered_choices() {
        let opt = make_select(
            "model",
            "gpt-5-codex",
            &[
                ("gpt-5-codex", "GPT-5 Codex"),
                ("gpt-5", "GPT-5"),
                ("o4-mini", "o4-mini"),
            ],
        );
        let out = synthesize(&[opt]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].config_id, "model");
        assert_eq!(out[0].choices.len(), 3);
        assert_eq!(out[0].choices[0].value, "gpt-5-codex");
        assert_eq!(out[0].choices[1].value, "gpt-5");
        assert_eq!(out[0].choices[2].value, "o4-mini");
    }

    #[test]
    fn empty_choices_omits_command() {
        let opt = make_select("model", "", &[]);
        assert!(synthesize(&[opt]).is_empty());
    }

    #[test]
    fn non_allowlisted_config_id_omitted() {
        let opt = make_select("mode", "auto", &[("auto", "Auto"), ("manual", "Manual")]);
        assert!(synthesize(&[opt]).is_empty());
    }

    #[test]
    fn multiple_allowlisted_returned_in_allowlist_order() {
        let effort = make_select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        );
        let model = make_select("model", "gpt-5", &[("gpt-5", "GPT-5")]);
        let out = synthesize(&[effort, model]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[1].name, "effort");
    }

    #[test]
    fn current_value_populated() {
        let opt = make_select("model", "gpt-5-codex", &[("gpt-5-codex", "GPT-5 Codex")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].current_value, Some("gpt-5-codex".to_string()));
    }

    #[test]
    fn hint_format_when_current_value_some() {
        let opt = make_select("model", "gpt-5-codex", &[("gpt-5-codex", "GPT-5 Codex")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].hint, Some("current: gpt-5-codex".to_string()));
    }

    #[test]
    fn renames_reasoning_effort_to_effort_at_slash_surface() {
        let opt = make_select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("high", "High")],
        );
        let out = synthesize(&[opt]);
        assert_eq!(out[0].name, "effort");
        assert_eq!(out[0].config_id, "reasoning_effort");
    }

    #[test]
    fn arg_picker_spec_is_config_option_typed() {
        let opt = make_select("model", "gpt-5", &[("gpt-5", "GPT-5")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].arg_picker_spec.free_text_hint, "");
        assert_eq!(
            out[0].arg_picker_spec.typed_hint,
            Some(ArgPickerHint::ConfigOption {
                config_id: "model".into()
            })
        );
    }

    #[test]
    fn extract_choices_returns_choices_in_advertised_order() {
        let opt = make_select(
            "model",
            "gpt-5",
            &[
                ("gpt-5-codex", "GPT-5 Codex"),
                ("gpt-5", "GPT-5"),
                ("o4-mini", "o4-mini"),
            ],
        );
        let choices = extract_choices(&opt);
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].value, "gpt-5-codex");
        assert_eq!(choices[0].label, "GPT-5 Codex");
        assert_eq!(choices[1].value, "gpt-5");
        assert_eq!(choices[2].value, "o4-mini");
    }

    #[test]
    fn extract_choices_empty_for_empty_select() {
        let opt = make_select("model", "", &[]);
        assert!(extract_choices(&opt).is_empty());
    }

    #[test]
    fn synthesize_advertised_from_models_only_emits_model_command() {
        let models = SessionModelState::new(
            ModelId::new("gpt-5"),
            vec![
                ModelInfo::new(ModelId::new("gpt-5"), "GPT-5"),
                ModelInfo::new(ModelId::new("o4-mini"), "o4-mini"),
            ],
        );
        let caps = make_caps(vec![], Some(models));

        let out = synthesize_advertised(&caps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].config_id, "model");
        assert_eq!(out[0].choices.len(), 2);
        assert_eq!(out[0].current_value, Some("gpt-5".to_string()));
    }

    #[test]
    fn synthesize_advertised_from_config_options_only_emits_existing_commands() {
        let caps = make_caps(
            vec![
                make_select(
                    "model",
                    "gpt-5",
                    &[
                        ("gpt-5", "GPT-5"),
                        ("gpt-5-codex", "GPT-5 Codex"),
                        ("o4-mini", "o4-mini"),
                    ],
                ),
                make_select(
                    "reasoning_effort",
                    "medium",
                    &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
                ),
            ],
            None,
        );

        let out = synthesize_advertised(&caps);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].choices.len(), 3);
        assert_eq!(out[1].name, "effort");
        assert_eq!(out[1].choices.len(), 3);
    }

    #[test]
    fn synthesize_advertised_from_both_prefers_config_options() {
        let models = SessionModelState::new(
            ModelId::new("model-from-model-state"),
            vec![
                ModelInfo::new(ModelId::new("model-from-model-state"), "Model State A"),
                ModelInfo::new(ModelId::new("model-state-b"), "Model State B"),
            ],
        );
        let caps = make_caps(
            vec![make_select(
                "model",
                "model-from-config-option",
                &[
                    ("model-from-config-option", "Config A"),
                    ("config-b", "Config B"),
                    ("config-c", "Config C"),
                ],
            )],
            Some(models),
        );

        let out = synthesize_advertised(&caps);
        let model_entries: Vec<&AdvertisedCommand> =
            out.iter().filter(|cmd| cmd.config_id == "model").collect();
        assert_eq!(model_entries.len(), 1);
        assert_eq!(model_entries[0].choices.len(), 3);
        assert_eq!(
            model_entries[0].choices[0].value,
            "model-from-config-option"
        );
    }

    #[test]
    fn synthesize_advertised_from_neither_emits_no_model_command() {
        let caps = make_caps(vec![], None);
        let out = synthesize_advertised(&caps);
        assert!(out.is_empty());
    }

    #[test]
    fn synthesize_advertised_with_empty_available_models_falls_through() {
        let models = SessionModelState::new(ModelId::new("gpt-5"), vec![]);
        let caps = make_caps(vec![], Some(models));
        let out = synthesize_advertised(&caps);
        assert!(out.is_empty());
    }
}
