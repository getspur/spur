//! Model/effort catalog synthesis tests for `spur_commands::advertised`
//! (decoupling spec 2026-09-23 §3.5, phase C4).
//!
//! [`ModelEffortCatalog::from_caps`] is the single grok / kiro /
//! standard-config-options precedence implementation: the TUI advertises
//! `/model` and `/effort` through it, and the notebook will map the same
//! catalog to `ChatSessionConfigOptions` at P2. Written RED-first against
//! the C4 API. The `*_entries_stay_identical_to_the_catalog` tests are the
//! parity gate: the advertised `/model` and `/effort` entries must stay
//! identical to the catalog's planes.
//!
//! Fixtures deliberately carry no capability evidence: `from_caps` reads
//! only the advertised data planes, and the no-evidence path of
//! `entries_from_caps` emits synthesized candidates verbatim, which keeps
//! the catalog ↔ entry parity exact.

use spur_acp::{
    AcpSessionId, AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion,
    SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SpurAgentCaps,
};
use spur_commands::advertised::{AdvertisedSource, CatalogChoice, ModelEffortCatalog};
use spur_commands::entry::{CommandEntry, CommandSource, Dispatch};

// ------------------------------------------------------------- fixtures

fn choice(value: &str, label: &str) -> CatalogChoice {
    CatalogChoice {
        value: value.to_owned(),
        label: label.to_owned(),
        description: None,
    }
}

fn select_option(config_id: &str, current: &str, choices: &[(&str, &str)]) -> SessionConfigOption {
    SessionConfigOption::select(
        SessionConfigId::new(config_id.to_owned()),
        format!("{config_id} label"),
        current.to_owned(),
        choices
            .iter()
            .map(|(value, name)| SessionConfigSelectOption::new((*value).to_owned(), *name))
            .collect::<Vec<_>>(),
    )
}

fn codex_caps_with_model_and_effort() -> SpurAgentCaps {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(AcpSessionId::new("codex-sid"));
    new.config_options = Some(vec![
        select_option(
            "model",
            "gpt-5-codex",
            &[("gpt-5-codex", "GPT-5 Codex"), ("gpt-5", "GPT-5")],
        ),
        select_option(
            "reasoning_effort",
            "medium",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        ),
    ]);
    SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp)
}

fn grok_caps() -> SpurAgentCaps {
    let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
    init.meta = Some(
        serde_json::json!({
            "modelState": {
                "currentModelId": "grok-4.6",
                "availableModels": [
                    {
                        "modelId": "grok-4.6",
                        "name": "Grok 4.6",
                        "_meta": {
                            "reasoningEffort": "high",
                            "reasoningEfforts": [
                                {"id": "xhigh", "label": "Extra High Effort"},
                                {"id": "high", "label": "High Effort"},
                                {"id": "medium", "label": "Medium Effort"},
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
        &NewSessionResponse::new(AcpSessionId::new("grok-sid")),
        AgentKind::Grok,
    )
}

fn kiro_caps() -> SpurAgentCaps {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(AcpSessionId::new("kiro-sid"));
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

/// Extract an entry's static picker choices as catalog choices so parity
/// is a field-for-field comparison against the catalog planes.
fn entry_choices(entry: &CommandEntry) -> Vec<CatalogChoice> {
    use spur_acp::adapter::arg_picker_hint::ArgPickerHint;
    let spec = entry
        .arg_picker_spec
        .as_ref()
        .expect("advertised /model and /effort entries must carry a picker");
    match spec.typed_hint.as_ref().expect("typed picker hint") {
        ArgPickerHint::StaticChoices { choices } => choices
            .iter()
            .map(|c| CatalogChoice {
                value: c.value.clone(),
                label: c.label.clone(),
                description: c.description.clone(),
            })
            .collect(),
        ArgPickerHint::ConfigOption { .. } => {
            panic!("expected static choices picker, got a config-option picker")
        }
    }
}

fn find_entry<'a>(entries: &'a [CommandEntry], name: &str) -> Option<&'a CommandEntry> {
    entries.iter().find(|entry| entry.name == name)
}

// ---------------------------------------------------- from_caps synthesis

#[test]
fn standard_config_options_feed_the_catalog() {
    let catalog = ModelEffortCatalog::from_caps(&codex_caps_with_model_and_effort());
    assert_eq!(
        catalog.models,
        vec![
            choice("gpt-5-codex", "GPT-5 Codex"),
            choice("gpt-5", "GPT-5")
        ]
    );
    assert_eq!(catalog.current_model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(
        catalog.efforts,
        vec![
            choice("low", "Low"),
            choice("medium", "Medium"),
            choice("high", "High")
        ]
    );
    assert_eq!(catalog.current_effort.as_deref(), Some("medium"));
}

#[test]
fn standard_choice_descriptions_flow_into_the_catalog() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(AcpSessionId::new("codex-sid"));
    new.config_options = Some(vec![SessionConfigOption::select(
        SessionConfigId::new("model"),
        "Model",
        "gpt-5-codex",
        vec![SessionConfigSelectOption::new("gpt-5-codex", "GPT-5 Codex")
            .description(Some("fastest coder".to_owned()))],
    )]);
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

    let catalog = ModelEffortCatalog::from_caps(&caps);
    assert_eq!(
        catalog.models,
        vec![CatalogChoice {
            value: "gpt-5-codex".to_owned(),
            label: "GPT-5 Codex".to_owned(),
            description: Some("fastest coder".to_owned()),
        }]
    );
}

#[test]
fn standard_plane_wins_over_vendor_planes() {
    let mut caps = grok_caps();
    caps.config_options = vec![select_option(
        "model",
        "test-model",
        &[("test-model", "Test Model")],
    )];

    let catalog = ModelEffortCatalog::from_caps(&caps);
    assert_eq!(
        catalog.models,
        vec![choice("test-model", "Test Model")],
        "the standard config-option plane must win the precedence over the grok display"
    );
    assert_eq!(catalog.current_model.as_deref(), Some("test-model"));
    assert!(catalog.efforts.is_empty());
}

#[test]
fn grok_display_feeds_model_scoped_effort_catalog() {
    let catalog = ModelEffortCatalog::from_caps(&grok_caps());
    assert_eq!(
        catalog.models,
        vec![
            choice("grok-4.6", "Grok 4.6"),
            choice("grok-composer-2.5-fast", "Grok Composer 2.5 Fast")
        ]
    );
    assert_eq!(catalog.current_model.as_deref(), Some("grok-4.6"));
    assert_eq!(
        catalog.efforts,
        vec![
            choice("xhigh", "Extra High Effort"),
            choice("high", "High Effort"),
            choice("medium", "Medium Effort"),
            choice("low", "Low Effort")
        ],
        "efforts must be scoped to the current model"
    );
    assert_eq!(catalog.current_effort.as_deref(), Some("high"));
}

#[test]
fn grok_model_changed_re_scopes_catalog_efforts() {
    let mut caps = grok_caps();
    assert!(caps.apply_grok_model_changed(&serde_json::json!({
        "sessionId": "grok-sid",
        "update": {
            "sessionUpdate": "model_changed",
            "model_id": "grok-composer-2.5-fast"
        }
    })));

    let catalog = ModelEffortCatalog::from_caps(&caps);
    assert_eq!(catalog.models.len(), 2, "model catalog survives a switch");
    assert_eq!(
        catalog.current_model.as_deref(),
        Some("grok-composer-2.5-fast")
    );
    assert!(
        catalog.efforts.is_empty(),
        "composer models advertise no efforts, so the effort plane empties"
    );
}

#[test]
fn kiro_recovered_models_feed_a_model_only_catalog() {
    let catalog = ModelEffortCatalog::from_caps(&kiro_caps());
    assert_eq!(
        catalog.models,
        vec![
            CatalogChoice {
                value: "auto".to_owned(),
                label: "auto".to_owned(),
                description: Some("task-picked".to_owned()),
            },
            CatalogChoice {
                value: "claude-sonnet-4.5".to_owned(),
                label: "claude-sonnet-4.5".to_owned(),
                description: Some("Claude Sonnet 4.5 model".to_owned()),
            },
        ]
    );
    assert_eq!(catalog.current_model.as_deref(), Some("claude-sonnet-4.5"));
    assert!(catalog.efforts.is_empty(), "Kiro has no effort surface");
    assert_eq!(catalog.current_effort, None);
}

#[test]
fn caps_without_catalog_planes_yield_an_empty_catalog() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let new = NewSessionResponse::new(AcpSessionId::new("sid"));
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);

    assert_eq!(
        ModelEffortCatalog::from_caps(&caps),
        ModelEffortCatalog::default()
    );
}

// --------------------------------------- advertised entry parity (the gate)

#[test]
fn grok_model_and_effort_entries_stay_identical_to_the_catalog() {
    let caps = grok_caps();
    let catalog = ModelEffortCatalog::from_caps(&caps);
    let entries = AdvertisedSource::entries_from_caps("grok", &caps);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["model", "effort"],
        "vendor entries exist exactly when the catalog planes are non-empty"
    );

    let model = find_entry(&entries, "model").expect("/model entry");
    assert_eq!(model.description, "Switch model for this session");
    assert_eq!(
        model.hint.as_deref(),
        Some("Grok 4.6"),
        "model hint is the current model's label"
    );
    assert!(matches!(
        &model.source,
        CommandSource::Advertised { handle } if handle == "grok"
    ));
    assert!(matches!(model.dispatch, Dispatch::SetSessionModel));
    assert_eq!(entry_choices(model), catalog.models);

    let effort = find_entry(&entries, "effort").expect("/effort entry");
    assert_eq!(effort.description, "Switch reasoning / thinking effort");
    assert_eq!(
        effort.hint.as_deref(),
        Some("High Effort"),
        "effort hint is the current effort's label"
    );
    assert!(matches!(effort.dispatch, Dispatch::SetSessionEffort));
    assert_eq!(entry_choices(effort), catalog.efforts);
}

#[test]
fn kiro_model_entries_stay_identical_to_the_catalog() {
    let caps = kiro_caps();
    let catalog = ModelEffortCatalog::from_caps(&caps);
    let entries = AdvertisedSource::entries_from_caps("kiro", &caps);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["model"]
    );

    let model = find_entry(&entries, "model").expect("/model entry");
    assert_eq!(model.description, "Switch model for this session");
    assert_eq!(model.hint.as_deref(), Some("claude-sonnet-4.5"));
    assert!(matches!(model.dispatch, Dispatch::SetSessionModel));
    assert_eq!(entry_choices(model), catalog.models);
    assert!(
        find_entry(&entries, "effort").is_none(),
        "Kiro advertises no effort plane"
    );
}

#[test]
fn standard_model_and_effort_entries_stay_identical_to_the_catalog() {
    let caps = codex_caps_with_model_and_effort();
    let catalog = ModelEffortCatalog::from_caps(&caps);
    let entries = AdvertisedSource::entries_from_caps("codex", &caps);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["model", "effort"]
    );

    let model = find_entry(&entries, "model").expect("/model entry");
    assert_eq!(
        model.hint.as_deref(),
        Some("current: gpt-5-codex"),
        "standard entries surface the catalog's current value as hint"
    );
    assert!(matches!(
        &model.dispatch,
        Dispatch::SetSessionConfigOption { config_id } if config_id == "model"
    ));
    assert_config_option_picker(model, "model");

    let effort = find_entry(&entries, "effort").expect("/effort entry");
    assert_eq!(effort.hint.as_deref(), Some("current: medium"));
    assert!(matches!(
        &effort.dispatch,
        Dispatch::SetSessionConfigOption { config_id } if config_id == "reasoning_effort"
    ));
    assert_config_option_picker(effort, "reasoning_effort");

    // Catalog ↔ entry parity for the standard plane: the currents the hint
    // surfaces are the catalog's currents.
    assert_eq!(catalog.current_model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(catalog.current_effort.as_deref(), Some("medium"));
    assert!(
        !catalog.models.is_empty() && !catalog.efforts.is_empty(),
        "entries exist exactly when the catalog planes are non-empty"
    );
}

/// A grok caps value that also carries a standard model config option
/// follows the shared precedence on the entry path too: `entries_from_caps`
/// emits the standard `/model` and never a direct `SetSessionModel` (or
/// vendor effort) entry from the grok display. This is the gate that
/// `vendor_from_caps` and `from_caps` share one rule.
#[test]
fn standard_model_option_on_grok_caps_suppresses_vendor_entries() {
    let mut caps = grok_caps();
    caps.config_options = vec![select_option(
        "model",
        "test-model",
        &[("test-model", "Test Model")],
    )];

    let catalog = ModelEffortCatalog::from_caps(&caps);
    assert_eq!(
        catalog.models,
        vec![choice("test-model", "Test Model")],
        "from_caps keeps the standard plane winning over the grok display"
    );
    assert!(catalog.efforts.is_empty());

    let entries = AdvertisedSource::entries_from_caps("grok", &caps);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["model"],
        "the grok vendor plane must not extend the entries once the standard plane won"
    );
    let model = find_entry(&entries, "model").expect("/model entry");
    assert!(matches!(
        &model.dispatch,
        Dispatch::SetSessionConfigOption { config_id } if config_id == "model"
    ));
    assert_config_option_picker(model, "model");
    assert!(
        entries.iter().all(|entry| !matches!(
            entry.dispatch,
            Dispatch::SetSessionModel | Dispatch::SetSessionEffort
        )),
        "no direct session-model / session-effort dispatch may coexist with the standard /model"
    );
}

/// Standard-plane entries read their live choices from the cached session
/// config snapshot (picker type `ConfigOption`), so their parity with the
/// catalog is the entry's existence, dispatch config id, and hint — the
/// choice lists themselves are pinned by `from_caps` against the same
/// `synthesize` output in the tests above.
fn assert_config_option_picker(entry: &CommandEntry, config_id: &str) {
    use spur_acp::adapter::arg_picker_hint::ArgPickerHint;
    let spec = entry
        .arg_picker_spec
        .as_ref()
        .expect("standard entries must carry a picker");
    assert!(
        matches!(spec.typed_hint.as_ref(), Some(ArgPickerHint::ConfigOption { config_id: id }) if id == config_id),
        "standard {config_id} entry must read live choices from the config snapshot"
    );
}
