//! Synthesizes interactive `AdvertisedCommand` rows from the agent's
//! `Vec<SessionConfigOption>`. Vendor-neutral by `config_id` allow-list.

use agent_client_protocol::schema::v1::{
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions,
};

use super::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};
use crate::SpurAgentCaps;

/// Vendor-neutral description of an interactive slash command synthesized from
/// the agent's advertised config options. spur-tui consumes this without
/// needing ACP schema imports.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedChoice {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Vendor-neutral allow-list for config options SPUR exposes as slash commands.
/// `slash_name` may differ from the fallback ACP config id (e.g.
/// `reasoning_effort` -> `effort` at the slash surface).
///
/// Entry order defines the stable slash-command order.
const ALLOW_LIST: &[AllowedConfigOption] = &[
    AllowedConfigOption {
        matcher: ConfigOptionMatcher::CategoryOrAbsentId {
            category: KnownConfigCategory::Model,
            fallback_config_id: "model",
        },
        slash_name: "model",
        slash_desc: "Switch model for this session",
    },
    AllowedConfigOption {
        matcher: ConfigOptionMatcher::CategoryOrAbsentId {
            category: KnownConfigCategory::ThoughtLevel,
            fallback_config_id: "reasoning_effort",
        },
        slash_name: "effort",
        slash_desc: "Switch reasoning / thinking effort",
    },
    AllowedConfigOption {
        matcher: ConfigOptionMatcher::ExactIdWithCategoryOrUnmapped {
            category: KnownConfigCategory::ModelConfig,
            config_id: "fast-mode",
        },
        slash_name: "fast",
        slash_desc: "Toggle Codex fast mode",
    },
];

struct AllowedConfigOption {
    matcher: ConfigOptionMatcher,
    slash_name: &'static str,
    slash_desc: &'static str,
}

#[derive(Clone, Copy)]
enum ConfigOptionMatcher {
    CategoryOrAbsentId {
        category: KnownConfigCategory,
        fallback_config_id: &'static str,
    },
    ExactIdWithCategoryOrUnmapped {
        category: KnownConfigCategory,
        config_id: &'static str,
    },
}

#[derive(Clone, Copy)]
enum KnownConfigCategory {
    Model,
    ModelConfig,
    ThoughtLevel,
}

/// Extract the selectable choices from a `SessionConfigOption`'s payload, in
/// advertised order. Grouped select payloads are flattened with group context
/// folded into each choice description. Used by the TUI to instantiate
/// `ConfigOptionQuerySource` from cached options.
pub fn extract_choices(opt: &SessionConfigOption) -> Vec<AdvertisedChoice> {
    match &opt.kind {
        SessionConfigKind::Select(select) => match &select.options {
            SessionConfigSelectOptions::Ungrouped(choices) => choices
                .iter()
                .map(|choice| advertised_choice(choice, None))
                .collect(),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| {
                    group
                        .options
                        .iter()
                        .map(|choice| advertised_choice(choice, Some(group.name.as_str())))
                })
                .collect(),
            _ => Vec::new(),
        },
        // Future kinds (Boolean, etc.) → no select choices.
        _ => Vec::new(),
    }
}

pub fn synthesize(options: &[SessionConfigOption]) -> Vec<AdvertisedCommand> {
    let mut out = Vec::new();
    for allowed in ALLOW_LIST {
        let Some(opt) = option_for_match(options, allowed.matcher) else {
            continue;
        };

        let SessionConfigKind::Select(select) = &opt.kind else {
            continue;
        };

        let choices = extract_choices(opt);
        if choices.is_empty() {
            continue;
        }

        let current = select.current_value.0.as_ref();
        let current_value = if current.is_empty() {
            None
        } else {
            Some(current.to_string())
        };
        let hint = current_value.as_ref().map(|v| format!("current: {v}"));

        out.push(AdvertisedCommand {
            name: allowed.slash_name.to_string(),
            description: allowed.slash_desc.to_string(),
            hint,
            config_id: opt.id.0.to_string(),
            current_value,
            choices,
            arg_picker_spec: ArgPickerSpec {
                free_text_hint: String::new(),
                typed_hint: Some(ArgPickerHint::ConfigOption {
                    config_id: opt.id.0.to_string(),
                }),
            },
        });
    }
    out
}

/// Synthesize advertised slash commands from frozen session capabilities.
///
/// ACP 1.0 expresses model selection through session config options, so this
/// is now a thin wrapper over [`synthesize`].
pub fn synthesize_advertised(caps: &SpurAgentCaps) -> Vec<AdvertisedCommand> {
    synthesize(&caps.config_options)
}

fn advertised_choice(
    choice: &SessionConfigSelectOption,
    group_name: Option<&str>,
) -> AdvertisedChoice {
    AdvertisedChoice {
        value: choice.value.0.to_string(),
        label: choice.name.clone(),
        description: grouped_description(group_name, choice.description.as_deref()),
    }
}

fn grouped_description(
    group_name: Option<&str>,
    choice_description: Option<&str>,
) -> Option<String> {
    match (
        group_name.filter(|name| !name.is_empty()),
        choice_description,
    ) {
        (Some(group), Some(description)) if !description.is_empty() => {
            Some(format!("{group}: {description}"))
        }
        (Some(group), _) => Some(group.to_string()),
        (None, Some(description)) => Some(description.to_string()),
        (None, None) => None,
    }
}

fn option_for_match(
    options: &[SessionConfigOption],
    matcher: ConfigOptionMatcher,
) -> Option<&SessionConfigOption> {
    match matcher {
        ConfigOptionMatcher::CategoryOrAbsentId {
            category,
            fallback_config_id,
        } => option_by_category_or_absent_id(options, category, fallback_config_id),
        ConfigOptionMatcher::ExactIdWithCategoryOrUnmapped {
            category,
            config_id,
        } => options.iter().find(|option| {
            option.id.0.as_ref() == config_id
                && (category_matches(option.category.as_ref(), category)
                    || category_is_absent_or_unmapped(option.category.as_ref()))
        }),
    }
}

fn option_by_category_or_absent_id<'a>(
    options: &'a [SessionConfigOption],
    category: KnownConfigCategory,
    fallback_id: &str,
) -> Option<&'a SessionConfigOption> {
    options
        .iter()
        .find(|option| category_matches(option.category.as_ref(), category))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.category.is_none() && option.id.0.as_ref() == fallback_id)
        })
}

fn category_matches(
    category: Option<&SessionConfigOptionCategory>,
    expected: KnownConfigCategory,
) -> bool {
    matches!(
        (expected, category),
        (
            KnownConfigCategory::Model,
            Some(SessionConfigOptionCategory::Model)
        ) | (
            KnownConfigCategory::ModelConfig,
            Some(SessionConfigOptionCategory::ModelConfig)
        ) | (
            KnownConfigCategory::ThoughtLevel,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        )
    )
}

fn category_is_absent_or_unmapped(category: Option<&SessionConfigOptionCategory>) -> bool {
    category.is_none() || matches!(category, Some(SessionConfigOptionCategory::Other(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentKind;
    use agent_client_protocol::schema::v1::{
        InitializeResponse, NewSessionResponse, SessionConfigId, SessionConfigKind,
        SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelect,
        SessionConfigSelectGroup, SessionConfigSelectOption, SessionConfigSelectOptions,
        SessionConfigValueId, SessionId,
    };
    use agent_client_protocol::schema::ProtocolVersion;

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

    fn make_caps(config_options: Vec<SessionConfigOption>) -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(SessionId::new("test-caps"));
        new.config_options = Some(config_options);
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
    fn fast_mode_id_fallback_emits_fast_command() {
        let fast = make_select("fast-mode", "on", &[("off", "Off"), ("on", "On")]);
        let caps = make_caps(vec![fast]);

        let out = synthesize_advertised(&caps);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "fast");
        assert_eq!(out[0].description, "Toggle Codex fast mode");
        assert_eq!(out[0].config_id, "fast-mode");
        assert_eq!(out[0].current_value.as_deref(), Some("on"));
        assert_eq!(out[0].choices.len(), 2);
        assert_eq!(
            out[0].arg_picker_spec.typed_hint,
            Some(ArgPickerHint::ConfigOption {
                config_id: "fast-mode".into()
            })
        );
    }

    #[test]
    fn fast_mode_id_fallback_accepts_unmapped_category() {
        let fast = make_select("fast-mode", "off", &[("off", "Off"), ("on", "On")]).category(
            SessionConfigOptionCategory::Other("future_model_config".to_string()),
        );

        let out = synthesize(&[fast]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "fast");
        assert_eq!(out[0].config_id, "fast-mode");
    }

    #[test]
    fn empty_fast_mode_choices_omit_command() {
        let fast =
            make_select("fast-mode", "", &[]).category(SessionConfigOptionCategory::ModelConfig);

        assert!(synthesize(&[fast]).is_empty());
    }

    #[test]
    fn unrelated_model_config_id_is_omitted() {
        let unrelated = make_select("temperature", "high", &[("high", "High")])
            .category(SessionConfigOptionCategory::ModelConfig);

        assert!(synthesize(&[unrelated]).is_empty());
    }

    #[test]
    fn multiple_allowlisted_returned_in_allowlist_order() {
        let effort = make_select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        )
        .category(SessionConfigOptionCategory::ThoughtLevel);
        let model = make_select("model", "gpt-5", &[("gpt-5", "GPT-5")])
            .category(SessionConfigOptionCategory::Model);
        let fast = make_select("fast-mode", "on", &[("off", "Off"), ("on", "On")])
            .category(SessionConfigOptionCategory::ModelConfig);

        let out = synthesize(&[fast, effort, model]);

        assert_eq!(out.len(), 3);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[1].name, "effort");
        assert_eq!(out[2].name, "fast");
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
    fn synthesize_prefers_model_category_over_model_id_allowlist() {
        let legacy = make_select("model", "legacy", &[("legacy", "Legacy")]);
        let categorized = make_select("vendor_model", "sonnet", &[("sonnet", "Sonnet")])
            .category(SessionConfigOptionCategory::Model);

        let out = synthesize(&[legacy, categorized]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].config_id, "vendor_model");
        assert_eq!(out[0].current_value.as_deref(), Some("sonnet"));
        assert_eq!(out[0].choices[0].label, "Sonnet");
    }

    #[test]
    fn synthesize_prefers_thought_level_category_for_effort() {
        let legacy = make_select("reasoning_effort", "low", &[("low", "Low")]);
        let categorized = make_select("thinking_level", "high", &[("high", "High")])
            .category(SessionConfigOptionCategory::ThoughtLevel);

        let out = synthesize(&[legacy, categorized]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "effort");
        assert_eq!(out[0].config_id, "thinking_level");
        assert_eq!(out[0].current_value.as_deref(), Some("high"));
        assert_eq!(out[0].choices[0].label, "High");
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
    fn extract_choices_flattens_grouped_selects_with_group_context() {
        let opt = SessionConfigOption::new(
            SessionConfigId::new("model"),
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new("gpt-5"),
                SessionConfigSelectOptions::Grouped(vec![
                    SessionConfigSelectGroup::new(
                        "openai",
                        "OpenAI",
                        vec![SessionConfigSelectOption::new("gpt-5", "GPT-5")
                            .description("General purpose")],
                    ),
                    SessionConfigSelectGroup::new(
                        "anthropic",
                        "Anthropic",
                        vec![SessionConfigSelectOption::new("sonnet", "Sonnet")],
                    ),
                ]),
            )),
        );

        let choices = extract_choices(&opt);

        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].value, "gpt-5");
        assert_eq!(choices[0].label, "GPT-5");
        assert_eq!(
            choices[0].description.as_deref(),
            Some("OpenAI: General purpose")
        );
        assert_eq!(choices[1].value, "sonnet");
        assert_eq!(choices[1].label, "Sonnet");
        assert_eq!(choices[1].description.as_deref(), Some("Anthropic"));
    }

    #[test]
    fn extract_choices_empty_for_empty_select() {
        let opt = make_select("model", "", &[]);
        assert!(extract_choices(&opt).is_empty());
    }

    #[test]
    fn synthesize_advertised_from_model_config_option_emits_model_command() {
        let caps = make_caps(vec![make_select(
            "model",
            "gpt-5",
            &[("gpt-5", "GPT-5"), ("o4-mini", "o4-mini")],
        )]);

        let out = synthesize_advertised(&caps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].config_id, "model");
        assert_eq!(out[0].choices.len(), 2);
        assert_eq!(out[0].current_value, Some("gpt-5".to_string()));
    }

    #[test]
    fn synthesize_advertised_from_config_options_only_emits_existing_commands() {
        let caps = make_caps(vec![
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
        ]);

        let out = synthesize_advertised(&caps);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].choices.len(), 3);
        assert_eq!(out[1].name, "effort");
        assert_eq!(out[1].choices.len(), 3);
    }

    #[test]
    fn synthesize_advertised_emits_single_model_config_option() {
        let caps = make_caps(vec![make_select(
            "model",
            "model-from-config-option",
            &[
                ("model-from-config-option", "Config A"),
                ("config-b", "Config B"),
                ("config-c", "Config C"),
            ],
        )]);

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
        let caps = make_caps(vec![]);
        let out = synthesize_advertised(&caps);
        assert!(out.is_empty());
    }

    #[test]
    fn synthesize_advertised_ignores_grok_read_only_meta_display() {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(SessionId::new("grok-read-only-display"));
        new.meta = serde_json::from_value(serde_json::json!({
            "x.ai/sessionConfig": {
                "options": [
                    {
                        "id": "grok-4.5",
                        "category": "model",
                        "label": "Grok 4.5",
                        "selected": true
                    },
                    {
                        "id": "high",
                        "category": "mode",
                        "label": "High Effort",
                        "selected": true
                    }
                ]
            }
        }))
        .expect("Grok session meta fixture must deserialize");
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Grok);

        assert_eq!(caps.current_model_label().as_deref(), Some("Grok 4.5"));
        assert_eq!(caps.current_effort_label().as_deref(), Some("High Effort"));
        assert!(synthesize_advertised(&caps).is_empty());
    }

    #[test]
    fn synthesize_advertised_with_empty_model_choices_falls_through() {
        let caps = make_caps(vec![make_select("model", "gpt-5", &[])]);
        let out = synthesize_advertised(&caps);
        assert!(out.is_empty());
    }
}
