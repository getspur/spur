//! Grok model and reasoning-effort state from proprietary ACP meta.
//!
//! This module never creates standard session config options. Its output is
//! kept separate so it cannot imply support for `session/set_config_option`.

use agent_client_protocol::schema::v1::Meta;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::AgentKind;

const SESSION_CONFIG_KEY: &str = "x.ai/sessionConfig";
const KNOWN_EFFORT_IDS: [&str; 3] = ["high", "medium", "low"];

/// One Grok reasoning-effort choice supported by a specific model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrokEffortChoice {
    /// Wire value sent as `_meta.reasoningEffort`.
    pub id: String,
    /// Human-readable picker label.
    pub label: String,
}

/// One Grok model advertised by the proprietary catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrokModelChoice {
    /// Wire value sent as `modelId`.
    pub id: String,
    /// Human-readable picker label.
    pub label: String,
    /// Efforts supported by this model. Empty means `/effort` is unavailable.
    #[serde(default)]
    pub efforts: Vec<GrokEffortChoice>,
}

/// Selected state and catalog derived from Grok proprietary meta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrokSessionDisplay {
    /// Display name for the selected model, falling back to its raw id.
    pub model_label: Option<String>,
    /// Display name for the selected reasoning effort, falling back to its id.
    pub effort_label: Option<String>,
    /// Raw selected model id, when Grok provides one.
    pub model_id: Option<String>,
    /// Raw selected effort id, when Grok provides one.
    pub effort_id: Option<String>,
    /// Real model ids and their model-specific reasoning efforts.
    #[serde(default)]
    pub models: Vec<GrokModelChoice>,
}

impl GrokSessionDisplay {
    /// Models advertised by Grok's proprietary catalog.
    #[must_use]
    pub fn models(&self) -> &[GrokModelChoice] {
        &self.models
    }

    /// Reasoning efforts supported by `model_id`.
    #[must_use]
    pub fn efforts_for_model(&self, model_id: &str) -> &[GrokEffortChoice] {
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .map_or(&[], |model| model.efforts.as_slice())
    }

    /// Apply Grok's `_x.ai/session_notification` `model_changed` payload.
    ///
    /// Returns `true` only when the payload has the proven notification shape.
    pub fn apply_model_changed(&mut self, params: &Value) -> bool {
        let Some(update) = params.get("update").and_then(Value::as_object) else {
            return false;
        };
        if update.get("sessionUpdate").and_then(Value::as_str) != Some("model_changed") {
            return false;
        }
        let Some(model_id) = update.get("model_id").and_then(non_empty_string) else {
            return false;
        };

        self.model_label = self
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.label.clone())
            .or_else(|| Some(model_id.clone()));
        self.model_id = Some(model_id.clone());

        let effort = update
            .get("reasoning_effort")
            .and_then(non_empty_string)
            .filter(|effort| is_known_effort_id(effort));
        self.effort_label = effort.as_ref().map(|effort| {
            self.efforts_for_model(&model_id)
                .iter()
                .find(|choice| choice.id == *effort)
                .map_or_else(|| effort.clone(), |choice| choice.label.clone())
        });
        self.effort_id = effort;
        true
    }
}

/// Extract read-only model and reasoning-effort labels from Grok ACP meta.
///
/// Session configuration metadata takes precedence over initialize-time model
/// state. Unknown `mode` options are deliberately ignored because Grok also
/// uses that category for concepts unrelated to reasoning effort.
#[must_use]
pub fn extract_grok_session_display(
    agent_kind: AgentKind,
    initialize_meta: Option<&Meta>,
    session_meta: Option<&Meta>,
) -> Option<GrokSessionDisplay> {
    if agent_kind != AgentKind::Grok {
        return None;
    }

    let session_model = selected_session_option(session_meta, "model");
    let session_effort = selected_session_option(session_meta, "mode").filter(is_known_effort);
    let model_state = initialize_meta
        .and_then(|meta| meta.get("modelState"))
        .and_then(Value::as_object);
    let state_model = model_state.and_then(selected_model_from_state);
    let state_effort = model_state.and_then(selected_effort_from_state);

    let model = session_model.or(state_model);
    let effort = session_effort.or(state_effort);
    let models = extract_model_catalog(model_state, session_meta, model.as_ref());
    let display = GrokSessionDisplay {
        model_label: model.as_ref().and_then(DisplayValue::preferred_label),
        effort_label: effort.as_ref().and_then(DisplayValue::preferred_label),
        model_id: model.and_then(|value| value.id),
        effort_id: effort.and_then(|value| value.id),
        models,
    };

    if display.model_label.is_none()
        && display.effort_label.is_none()
        && display.model_id.is_none()
        && display.effort_id.is_none()
        && display.models.is_empty()
    {
        None
    } else {
        Some(display)
    }
}

#[derive(Debug)]
struct DisplayValue {
    id: Option<String>,
    label: Option<String>,
}

impl DisplayValue {
    fn from_object(value: &Map<String, Value>, id_key: &str, label_key: &str) -> Option<Self> {
        let id = value.get(id_key).and_then(non_empty_string);
        let label = value.get(label_key).and_then(non_empty_string);
        (id.is_some() || label.is_some()).then_some(Self { id, label })
    }

    fn preferred_label(&self) -> Option<String> {
        self.label.clone().or_else(|| self.id.clone())
    }
}

fn selected_session_option(session_meta: Option<&Meta>, category: &str) -> Option<DisplayValue> {
    session_meta?
        .get(SESSION_CONFIG_KEY)?
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find(|option| {
            option.get("category").and_then(Value::as_str) == Some(category)
                && option.get("selected").and_then(Value::as_bool) == Some(true)
        })
        .and_then(|option| DisplayValue::from_object(option, "id", "label"))
}

fn session_options(session_meta: Option<&Meta>, category: &str) -> Vec<DisplayValue> {
    session_meta
        .and_then(|meta| meta.get(SESSION_CONFIG_KEY))
        .and_then(|config| config.get("options"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|option| option.get("category").and_then(Value::as_str) == Some(category))
        .filter_map(|option| DisplayValue::from_object(option, "id", "label"))
        .collect()
}

fn extract_model_catalog(
    model_state: Option<&Map<String, Value>>,
    session_meta: Option<&Meta>,
    selected_model: Option<&DisplayValue>,
) -> Vec<GrokModelChoice> {
    let state_models = model_state
        .and_then(|state| state.get("availableModels"))
        .and_then(Value::as_array);
    let mut models = Vec::new();

    for option in session_options(session_meta, "model") {
        let Some(id) = option.id else {
            continue;
        };
        let state_model = state_models.and_then(|state_models| {
            state_models
                .iter()
                .filter_map(Value::as_object)
                .find(|model| model.get("modelId").and_then(Value::as_str) == Some(id.as_str()))
        });
        let label = option
            .label
            .or_else(|| {
                state_model
                    .and_then(|model| model.get("name"))
                    .and_then(non_empty_string)
            })
            .unwrap_or_else(|| id.clone());
        models.push(GrokModelChoice {
            id,
            label,
            efforts: state_model.map_or_else(Vec::new, efforts_from_model),
        });
    }

    if let Some(state_models) = state_models {
        for state_model in state_models.iter().filter_map(Value::as_object) {
            let Some(id) = state_model.get("modelId").and_then(non_empty_string) else {
                continue;
            };
            if models.iter().any(|model| model.id == id) {
                continue;
            }
            let label = state_model
                .get("name")
                .and_then(non_empty_string)
                .unwrap_or_else(|| id.clone());
            models.push(GrokModelChoice {
                id,
                label,
                efforts: efforts_from_model(state_model),
            });
        }
    }

    if state_models.is_none() {
        if let Some(selected_model_id) = selected_model.and_then(|model| model.id.as_deref()) {
            let session_efforts = session_options(session_meta, "mode")
                .into_iter()
                .filter_map(effort_choice)
                .collect::<Vec<_>>();
            if let Some(model) = models
                .iter_mut()
                .find(|model| model.id == selected_model_id)
            {
                model.efforts = session_efforts;
            }
        }
    }

    models
}

fn efforts_from_model(model: &Map<String, Value>) -> Vec<GrokEffortChoice> {
    model
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("reasoningEfforts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|effort| DisplayValue::from_object(effort, "id", "label"))
        .filter_map(effort_choice)
        .collect()
}

fn effort_choice(value: DisplayValue) -> Option<GrokEffortChoice> {
    let id = value.id.filter(|id| is_known_effort_id(id))?;
    let label = value.label.unwrap_or_else(|| id.clone());
    Some(GrokEffortChoice { id, label })
}

fn selected_model_from_state(model_state: &Map<String, Value>) -> Option<DisplayValue> {
    let current_model_id = model_state
        .get("currentModelId")
        .and_then(non_empty_string)?;
    let label = current_model(model_state, &current_model_id)
        .and_then(|model| model.get("name"))
        .and_then(non_empty_string);

    Some(DisplayValue {
        id: Some(current_model_id),
        label,
    })
}

fn selected_effort_from_state(model_state: &Map<String, Value>) -> Option<DisplayValue> {
    let current_model_id = model_state
        .get("currentModelId")
        .and_then(non_empty_string)?;
    let model_meta = current_model(model_state, &current_model_id)?
        .get("_meta")?
        .as_object()?;
    let effort_id = model_meta
        .get("reasoningEffort")
        .and_then(non_empty_string)?;
    let label = model_meta
        .get("reasoningEfforts")
        .and_then(Value::as_array)
        .and_then(|efforts| {
            efforts
                .iter()
                .filter_map(Value::as_object)
                .find(|effort| effort.get("id").and_then(Value::as_str) == Some(effort_id.as_str()))
        })
        .and_then(|effort| effort.get("label"))
        .and_then(non_empty_string);

    Some(DisplayValue {
        id: Some(effort_id),
        label,
    })
}

fn current_model<'a>(
    model_state: &'a Map<String, Value>,
    current_model_id: &str,
) -> Option<&'a Map<String, Value>> {
    model_state
        .get("availableModels")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find(|model| model.get("modelId").and_then(Value::as_str) == Some(current_model_id))
}

fn is_known_effort(value: &DisplayValue) -> bool {
    value.id.as_deref().is_some_and(is_known_effort_id)
}

fn is_known_effort_id(id: &str) -> bool {
    KNOWN_EFFORT_IDS
        .iter()
        .any(|known| id.eq_ignore_ascii_case(known))
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::Meta;
    use serde_json::{json, Value};

    use super::extract_grok_session_display;
    use crate::types::AgentKind;

    fn meta(value: Value) -> Meta {
        value
            .as_object()
            .expect("test metadata must be a JSON object")
            .clone()
    }

    #[test]
    fn session_config_selected_model_and_effort_override_model_state() {
        let initialize_meta = meta(json!({
            "modelState": {
                "currentModelId": "grok-composer-2.5-fast",
                "availableModels": [{
                    "modelId": "grok-composer-2.5-fast",
                    "name": "Grok Composer 2.5 Fast",
                    "_meta": {
                        "reasoningEffort": "low",
                        "reasoningEfforts": [{"id": "low", "label": "Low Effort"}]
                    }
                }]
            }
        }));
        let session_meta = meta(json!({
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
        }));

        let display = extract_grok_session_display(
            AgentKind::Grok,
            Some(&initialize_meta),
            Some(&session_meta),
        );

        let display = display.expect("Grok display should be extracted");
        assert_eq!(display.model_label.as_deref(), Some("Grok 4.5"));
        assert_eq!(display.effort_label.as_deref(), Some("High Effort"));
        assert_eq!(display.model_id.as_deref(), Some("grok-4.5"));
        assert_eq!(display.effort_id.as_deref(), Some("high"));
    }

    #[test]
    fn model_state_resolves_display_names_and_reasoning_effort_label() {
        let initialize_meta = meta(json!({
            "modelState": {
                "currentModelId": "grok-4.5",
                "availableModels": [{
                    "modelId": "grok-4.5",
                    "name": "Grok 4.5",
                    "_meta": {
                        "reasoningEffort": "medium",
                        "reasoningEfforts": [
                            {"id": "high", "label": "High Effort"},
                            {"id": "medium", "label": "Medium Effort"},
                            {"id": "low", "label": "Low Effort"}
                        ]
                    }
                }]
            }
        }));

        let display = extract_grok_session_display(AgentKind::Grok, Some(&initialize_meta), None);

        let display = display.expect("Grok display should be extracted");
        assert_eq!(display.model_label.as_deref(), Some("Grok 4.5"));
        assert_eq!(display.effort_label.as_deref(), Some("Medium Effort"));
        assert_eq!(display.model_id.as_deref(), Some("grok-4.5"));
        assert_eq!(display.effort_id.as_deref(), Some("medium"));
    }

    #[test]
    fn advertised_reasoning_efforts_are_not_limited_to_known_ids() {
        let initialize_meta = meta(json!({
            "modelState": {
                "currentModelId": "grok-4.6",
                "availableModels": [{
                    "modelId": "grok-4.6",
                    "name": "Grok 4.6",
                    "_meta": {
                        "reasoningEffort": "xhigh",
                        "reasoningEfforts": [
                            {"id": "xhigh", "label": "Extra High Effort"},
                            {"id": "high", "label": "High Effort"},
                            {"id": "future-effort", "label": "Future Effort"}
                        ]
                    }
                }]
            }
        }));

        let display = extract_grok_session_display(AgentKind::Grok, Some(&initialize_meta), None)
            .expect("Grok display should retain its advertised effort catalog");

        assert_eq!(display.effort_id.as_deref(), Some("xhigh"));
        assert_eq!(display.effort_label.as_deref(), Some("Extra High Effort"));
        assert_eq!(
            display
                .efforts_for_model("grok-4.6")
                .iter()
                .map(|choice| (choice.id.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("xhigh", "Extra High Effort"),
                ("high", "High Effort"),
                ("future-effort", "Future Effort"),
            ]
        );
    }

    #[test]
    fn unselected_session_field_falls_back_to_model_state_independently() {
        let initialize_meta = meta(json!({
            "modelState": {
                "currentModelId": "grok-4.5",
                "availableModels": [{
                    "modelId": "grok-4.5",
                    "name": "Grok 4.5",
                    "_meta": {"reasoningEffort": "low"}
                }]
            }
        }));
        let session_meta = meta(json!({
            "x.ai/sessionConfig": {
                "options": [
                    {
                        "id": "grok-composer-2.5-fast",
                        "category": "model",
                        "label": "Grok Composer 2.5 Fast",
                        "selected": false
                    },
                    {
                        "id": "high",
                        "category": "mode",
                        "label": "High Effort",
                        "selected": true
                    }
                ]
            }
        }));

        let display = extract_grok_session_display(
            AgentKind::Grok,
            Some(&initialize_meta),
            Some(&session_meta),
        );

        let display = display.expect("Grok display should be extracted");
        assert_eq!(display.model_label.as_deref(), Some("Grok 4.5"));
        assert_eq!(display.effort_label.as_deref(), Some("High Effort"));
        assert_eq!(display.model_id.as_deref(), Some("grok-4.5"));
        assert_eq!(display.effort_id.as_deref(), Some("high"));
    }

    #[test]
    fn unknown_selected_mode_is_not_treated_as_effort() {
        let session_meta = meta(json!({
            "x.ai/sessionConfig": {
                "options": [
                    {
                        "id": "grok-4.5",
                        "category": "model",
                        "selected": true
                    },
                    {
                        "id": "fast",
                        "category": "mode",
                        "label": "Fast Mode",
                        "selected": true
                    }
                ]
            }
        }));

        let display = extract_grok_session_display(AgentKind::Grok, None, Some(&session_meta));

        let display = display.expect("Grok model should still be extracted");
        assert_eq!(display.model_label.as_deref(), Some("grok-4.5"));
        assert_eq!(display.effort_label, None);
        assert_eq!(display.model_id.as_deref(), Some("grok-4.5"));
        assert_eq!(display.effort_id, None);
    }

    #[test]
    fn known_reasoning_effort_label_does_not_allow_unknown_mode_id() {
        let session_meta = meta(json!({
            "x.ai/sessionConfig": {
                "options": [{
                    "id": "reasoning-max",
                    "category": "mode",
                    "label": "High Effort",
                    "selected": true
                }]
            }
        }));

        let display = extract_grok_session_display(AgentKind::Grok, None, Some(&session_meta));

        assert_eq!(display, None);
    }

    #[test]
    fn malformed_metadata_and_non_grok_agents_return_none() {
        let malformed_initialize = meta(json!({"modelState": "not-an-object"}));
        let malformed_session = meta(json!({"x.ai/sessionConfig": {"options": "not-an-array"}}));

        assert_eq!(
            extract_grok_session_display(
                AgentKind::Grok,
                Some(&malformed_initialize),
                Some(&malformed_session),
            ),
            None
        );
        assert_eq!(
            extract_grok_session_display(
                AgentKind::CodexAcp,
                Some(&malformed_initialize),
                Some(&malformed_session),
            ),
            None
        );
    }

    #[test]
    fn retains_model_catalog_and_model_specific_efforts() {
        let initialize_meta = meta(json!({
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
        }));
        let session_meta = meta(json!({
            "x.ai/sessionConfig": {
                "options": [
                    {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                    {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer", "selected": false},
                    {"id": "high", "category": "mode", "label": "High Effort", "selected": true},
                    {"id": "medium", "category": "mode", "label": "Medium Effort", "selected": false},
                    {"id": "low", "category": "mode", "label": "Low Effort", "selected": false}
                ]
            }
        }));

        let display = extract_grok_session_display(
            AgentKind::Grok,
            Some(&initialize_meta),
            Some(&session_meta),
        )
        .expect("Grok catalog should be extracted");

        assert_eq!(
            display
                .models()
                .iter()
                .map(|model| (model.id.as_str(), model.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("grok-4.5", "Grok 4.5"),
                ("grok-composer-2.5-fast", "Composer"),
            ]
        );
        assert_eq!(display.efforts_for_model("grok-4.5").len(), 3);
        assert!(
            display
                .efforts_for_model("grok-composer-2.5-fast")
                .is_empty(),
            "composer must not advertise broken effort choices"
        );
    }

    #[test]
    fn model_changed_refreshes_selected_labels_and_clears_missing_effort() {
        let initialize_meta = meta(json!({
            "modelState": {
                "currentModelId": "grok-4.5",
                "availableModels": [
                    {
                        "modelId": "grok-4.5",
                        "name": "Grok 4.5",
                        "_meta": {
                            "reasoningEffort": "high",
                            "reasoningEfforts": [{"id": "high", "label": "High Effort"}]
                        }
                    },
                    {
                        "modelId": "grok-composer-2.5-fast",
                        "name": "Grok Composer 2.5 Fast",
                        "_meta": {"reasoningEfforts": []}
                    }
                ]
            }
        }));
        let mut display =
            extract_grok_session_display(AgentKind::Grok, Some(&initialize_meta), None)
                .expect("Grok display should be extracted");

        assert!(display.apply_model_changed(&json!({
            "sessionId": "sid",
            "update": {
                "sessionUpdate": "model_changed",
                "model_id": "grok-composer-2.5-fast"
            }
        })));
        assert_eq!(display.model_id.as_deref(), Some("grok-composer-2.5-fast"));
        assert_eq!(
            display.model_label.as_deref(),
            Some("Grok Composer 2.5 Fast")
        );
        assert_eq!(display.effort_id, None);
        assert_eq!(display.effort_label, None);
    }
}
