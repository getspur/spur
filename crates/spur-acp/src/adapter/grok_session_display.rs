//! Read-only Grok model and reasoning-effort labels from proprietary ACP meta.
//!
//! This module never creates standard session config options. Its output is
//! display-only so it cannot imply support for model or effort switching.

use agent_client_protocol::schema::v1::Meta;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::AgentKind;

const SESSION_CONFIG_KEY: &str = "x.ai/sessionConfig";
const KNOWN_EFFORT_IDS: [&str; 3] = ["high", "medium", "low"];
const KNOWN_EFFORT_LABELS: [&str; 3] = ["High Effort", "Medium Effort", "Low Effort"];

/// Labels derived from Grok proprietary meta.
///
/// These values are frozen when a session is created or loaded and are never
/// used to advertise `set_*` support or synthesize interactive slash commands.
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
    let display = GrokSessionDisplay {
        model_label: model.as_ref().and_then(DisplayValue::preferred_label),
        effort_label: effort.as_ref().and_then(DisplayValue::preferred_label),
        model_id: model.and_then(|value| value.id),
        effort_id: effort.and_then(|value| value.id),
    };

    if display.model_label.is_none()
        && display.effort_label.is_none()
        && display.model_id.is_none()
        && display.effort_id.is_none()
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
    value.id.as_deref().is_some_and(|id| {
        KNOWN_EFFORT_IDS
            .iter()
            .any(|known| id.eq_ignore_ascii_case(known))
    }) || value.label.as_deref().is_some_and(|label| {
        KNOWN_EFFORT_LABELS
            .iter()
            .any(|known| label.eq_ignore_ascii_case(known))
    })
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

    use super::{extract_grok_session_display, GrokSessionDisplay};
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

        assert_eq!(
            display,
            Some(GrokSessionDisplay {
                model_label: Some("Grok 4.5".to_owned()),
                effort_label: Some("High Effort".to_owned()),
                model_id: Some("grok-4.5".to_owned()),
                effort_id: Some("high".to_owned()),
            })
        );
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

        assert_eq!(
            display,
            Some(GrokSessionDisplay {
                model_label: Some("Grok 4.5".to_owned()),
                effort_label: Some("Medium Effort".to_owned()),
                model_id: Some("grok-4.5".to_owned()),
                effort_id: Some("medium".to_owned()),
            })
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

        assert_eq!(
            display,
            Some(GrokSessionDisplay {
                model_label: Some("Grok 4.5".to_owned()),
                effort_label: Some("High Effort".to_owned()),
                model_id: Some("grok-4.5".to_owned()),
                effort_id: Some("high".to_owned()),
            })
        );
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

        assert_eq!(
            display,
            Some(GrokSessionDisplay {
                model_label: Some("grok-4.5".to_owned()),
                effort_label: None,
                model_id: Some("grok-4.5".to_owned()),
                effort_id: None,
            })
        );
    }

    #[test]
    fn known_reasoning_effort_label_allows_vendor_specific_mode_id() {
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

        assert_eq!(
            display,
            Some(GrokSessionDisplay {
                model_label: None,
                effort_label: Some("High Effort".to_owned()),
                model_id: None,
                effort_id: Some("reasoning-max".to_owned()),
            })
        );
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
}
