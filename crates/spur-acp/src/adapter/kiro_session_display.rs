//! Kiro model catalog recovered from the top-level ACP `models` plane.
//!
//! ACP schema 1.1 dropped `NewSessionResponse.models`, so Kiro's live catalog
//! (`availableModels` + `currentModelId`) is stripped on typed deserialize.
//! `NativeAcpConnection` re-issues `session/new`/`session/load` as an
//! `ExtMethodRequest` for Kiro, injects the recovered plane under
//! [`RECOVERED_MODELS_META_KEY`], and this module freezes it into caps.
//!
//! Never creates standard session config options — Kiro has no
//! `session/set_config_option`, so slash `/model` must use DirectSetModel.

use agent_client_protocol::schema::v1::Meta;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::AgentKind;

/// Meta key written by the native connection when recovering the models plane.
pub const RECOVERED_MODELS_META_KEY: &str = "spur.recoveredModels";

/// One model advertised on Kiro's recovered models plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KiroModelChoice {
    /// Wire value sent as `modelId` to `session/set_model`.
    pub id: String,
    /// Human-readable picker / status label.
    pub label: String,
    /// Optional description from the agent catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Selected state and catalog derived from Kiro's recovered models plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KiroSessionDisplay {
    /// Display name for the selected model, falling back to its raw id.
    pub model_label: Option<String>,
    /// Raw selected model id.
    pub model_id: Option<String>,
    /// Real model ids from the recovered catalog.
    #[serde(default)]
    pub models: Vec<KiroModelChoice>,
}

impl KiroSessionDisplay {
    /// Models advertised by the recovered catalog.
    #[must_use]
    pub fn models(&self) -> &[KiroModelChoice] {
        &self.models
    }

    /// Update selection after a successful `session/set_model`.
    ///
    /// Returns `true` when `model_id` is in the catalog (or when the catalog
    /// is empty and we still accept the raw id for free-text recovery).
    pub fn apply_selected_model(&mut self, model_id: &str) -> bool {
        if model_id.is_empty() {
            return false;
        }
        let label = self
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.label.clone())
            .or_else(|| Some(model_id.to_owned()));
        self.model_id = Some(model_id.to_owned());
        self.model_label = label;
        true
    }
}

/// Extract Kiro model catalog + selection from recovered session meta.
///
/// Returns `None` when `agent_kind != Kiro` or no usable models key is present.
#[must_use]
pub fn extract_kiro_session_display(
    agent_kind: AgentKind,
    session_meta: Option<&Meta>,
) -> Option<KiroSessionDisplay> {
    if agent_kind != AgentKind::Kiro {
        return None;
    }
    let models_value = session_meta.and_then(|meta| meta.get(RECOVERED_MODELS_META_KEY))?;
    let models_obj = models_value.as_object()?;

    let available = models_obj
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|arr| arr.as_slice())
        .unwrap_or(&[]);

    let mut models = Vec::new();
    for entry in available {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let Some(id) = obj.get("modelId").and_then(non_empty_string) else {
            continue;
        };
        let label = obj
            .get("name")
            .and_then(non_empty_string)
            .unwrap_or_else(|| id.clone());
        let description = obj.get("description").and_then(non_empty_string);
        models.push(KiroModelChoice {
            id,
            label,
            description,
        });
    }

    let current_id = models_obj.get("currentModelId").and_then(non_empty_string);

    let model_label = current_id.as_ref().map(|id| {
        models
            .iter()
            .find(|model| model.id == *id)
            .map(|model| model.label.clone())
            .unwrap_or_else(|| id.clone())
    });

    let display = KiroSessionDisplay {
        model_label,
        model_id: current_id,
        models,
    };

    if display.model_label.is_none() && display.model_id.is_none() && display.models.is_empty() {
        None
    } else {
        Some(display)
    }
}

/// Inject a recovered models plane into session response meta.
///
/// Used by the native connection after a raw `session/new` / `session/load`
/// response is parsed as `serde_json::Value`.
pub fn inject_recovered_models_meta(meta: &mut Meta, models: Value) {
    meta.insert(RECOVERED_MODELS_META_KEY.to_string(), models);
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta_with_models(models: Value) -> Meta {
        let mut meta = Meta::new();
        inject_recovered_models_meta(&mut meta, models);
        meta
    }

    #[test]
    fn extracts_live_kiro_catalog_and_current_label() {
        let meta = meta_with_models(json!({
            "availableModels": [
                {
                    "modelId": "auto",
                    "name": "auto",
                    "description": "Models chosen by task"
                },
                {
                    "modelId": "claude-sonnet-4.5",
                    "name": "claude-sonnet-4.5",
                    "description": "Claude Sonnet 4.5 model"
                }
            ],
            "currentModelId": "claude-sonnet-4.5"
        }));

        let display = extract_kiro_session_display(AgentKind::Kiro, Some(&meta))
            .expect("Kiro display must extract");
        assert_eq!(display.model_id.as_deref(), Some("claude-sonnet-4.5"));
        assert_eq!(display.model_label.as_deref(), Some("claude-sonnet-4.5"));
        assert_eq!(display.models().len(), 2);
        assert_eq!(display.models()[0].id, "auto");
        assert_eq!(
            display.models()[1].description.as_deref(),
            Some("Claude Sonnet 4.5 model")
        );
    }

    #[test]
    fn non_kiro_agents_return_none() {
        let meta = meta_with_models(json!({
            "availableModels": [{"modelId": "x", "name": "X"}],
            "currentModelId": "x"
        }));
        assert!(extract_kiro_session_display(AgentKind::Grok, Some(&meta)).is_none());
        assert!(extract_kiro_session_display(AgentKind::CodexAcp, Some(&meta)).is_none());
    }

    #[test]
    fn missing_recovered_key_returns_none() {
        let meta = Meta::new();
        assert!(extract_kiro_session_display(AgentKind::Kiro, Some(&meta)).is_none());
        assert!(extract_kiro_session_display(AgentKind::Kiro, None).is_none());
    }

    #[test]
    fn apply_selected_model_updates_label_from_catalog() {
        let meta = meta_with_models(json!({
            "availableModels": [
                {"modelId": "auto", "name": "Auto"},
                {"modelId": "glm-5", "name": "GLM-5"}
            ],
            "currentModelId": "auto"
        }));
        let mut display =
            extract_kiro_session_display(AgentKind::Kiro, Some(&meta)).expect("display");
        assert!(display.apply_selected_model("glm-5"));
        assert_eq!(display.model_id.as_deref(), Some("glm-5"));
        assert_eq!(display.model_label.as_deref(), Some("GLM-5"));
    }
}
