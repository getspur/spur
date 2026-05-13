//! `SpurAgentCaps` — frozen-per-session capability cache.
//!
//! See `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md` §6.1.
//!
//! Wraps two wire facts: the agent's `AgentCapabilities` (from
//! `InitializeResponse`) and the `NewSessionResponse` payload's
//! `modes` / `models` / `config_options`. Spur derives `set_*` support
//! from session-create state because ACP 0.12 does not gate these
//! protocol-stable methods on `AgentCapabilities` flags.
//!
//! Named `SpurAgentCaps` (not `SessionCapabilities`) to avoid collision
//! with the SDK's `SessionCapabilities` struct that lives on
//! `AgentCapabilities`.

use agent_client_protocol::schema::{
    AgentCapabilities, InitializeResponse, NewSessionResponse, SessionConfigKind,
    SessionConfigOption, SessionConfigSelectOptions, SessionModeState, SessionModelState,
};
use serde::{Deserialize, Serialize};

use crate::types::AgentKind;

/// What the agent told spur during `initialize` + `session/new`.
/// Captured ONCE per session at session-create and frozen for the
/// session lifetime — ACP 0.12 has no protocol affordance for
/// mid-session capability renegotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurAgentCaps {
    /// Verbatim `AgentCapabilities` from `InitializeResponse`. Read its
    /// fields directly; future protocol additions land here automatically.
    pub agent: AgentCapabilities,
    /// `NewSessionResponse.modes` (or `LoadSessionResponse.modes`). Some
    /// state with non-empty `available_modes` ⇒ `session/set_mode` is usable.
    pub modes: Option<SessionModeState>,
    /// `NewSessionResponse.models`. Some(_) ⇒ `session/set_model` is usable.
    pub models: Option<SessionModelState>,
    /// `NewSessionResponse.config_options`. Non-empty ⇒
    /// `session/set_config_option` is usable.
    pub config_options: Vec<SessionConfigOption>,
    /// Agent identity captured from config at session creation.
    pub agent_kind: AgentKind,
}

impl SpurAgentCaps {
    /// Build from the two relevant wire responses.
    #[must_use]
    pub fn new(
        initialize: &InitializeResponse,
        new_session: &NewSessionResponse,
        agent_kind: AgentKind,
    ) -> Self {
        Self {
            agent: initialize.agent_capabilities.clone(),
            modes: new_session.modes.clone(),
            models: new_session.models.clone(),
            config_options: new_session.config_options.clone().unwrap_or_default(),
            agent_kind,
        }
    }

    /// `session/set_mode` is usable when the session has modes to switch between.
    #[must_use]
    pub fn supports_set_mode(&self) -> bool {
        self.modes
            .as_ref()
            .is_some_and(|m| !m.available_modes.is_empty())
    }

    /// `session/set_model` is usable when the session advertises a non-empty
    /// `available_models` list. Mirrors `supports_set_mode`'s `has_choices()`
    /// semantic — `Some(state)` with zero available models is not a usable
    /// model-switch surface, so the picker is hidden.
    #[must_use]
    pub fn supports_set_model(&self) -> bool {
        self.models
            .as_ref()
            .is_some_and(|m| !m.available_models.is_empty())
    }

    /// `session/set_config_option` is usable when the session advertises
    /// non-empty `config_options`.
    #[must_use]
    pub fn supports_set_config_option(&self) -> bool {
        !self.config_options.is_empty()
    }

    /// `session/load` is announced explicitly on `AgentCapabilities`.
    #[must_use]
    pub fn supports_load_session(&self) -> bool {
        self.agent.load_session
    }

    /// Display label for the active model.
    #[must_use]
    pub fn current_model_label(&self) -> Option<String> {
        let models = self.models.as_ref()?;
        Some(
            models
                .available_models
                .iter()
                .find(|info| info.model_id.0.as_ref() == models.current_model_id.0.as_ref())
                .map(|info| info.name.clone())
                .unwrap_or_else(|| models.current_model_id.0.to_string()),
        )
    }

    /// Display label for the active model from a config-options snapshot.
    /// Callers with live session state should pass that fresh snapshot
    /// instead of the frozen caps copy captured at session init.
    #[must_use]
    pub fn model_label_from_config_options(options: &[SessionConfigOption]) -> Option<&str> {
        let option = options
            .iter()
            .find(|option| option.id.0.as_ref() == "model")?;

        let select = match &option.kind {
            SessionConfigKind::Select(select) => select,
            _ => return None,
        };
        let current = select.current_value.0.as_ref();
        match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.as_str())
                .or(Some(current)),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.as_str())
                .or(Some(current)),
            _ => Some(current),
        }
    }

    /// Display label for the active reasoning effort from a config-options
    /// snapshot. Callers with live session state should pass that fresh
    /// snapshot instead of the frozen caps copy captured at session init.
    #[must_use]
    pub fn effort_label_from(options: &[SessionConfigOption]) -> Option<String> {
        let option = options
            .iter()
            .find(|option| option.id.0.as_ref() == "reasoning_effort")?;

        let select = match &option.kind {
            SessionConfigKind::Select(select) => select,
            _ => return None,
        };
        let current = select.current_value.0.as_ref();
        let name = match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.clone()),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.clone()),
            _ => None,
        };

        Some(name.unwrap_or_else(|| current.to_string()))
    }

    /// Display label for the active reasoning effort from this caps snapshot.
    /// This is the initial session state only; TUI status rendering should
    /// prefer live `BrainSession.config_options` snapshots when available.
    #[must_use]
    pub fn current_effort_label(&self) -> Option<String> {
        Self::effort_label_from(&self.config_options)
    }

    /// Whether this agent is expected to emit usage updates.
    #[must_use]
    pub fn usage_supported(&self) -> bool {
        crate::agent_quirks::usage_emit_default(self.agent_kind)
    }

    /// Probe a vendor `_meta` extension key (e.g. `"terminal_output"`).
    /// Returns false for missing keys, non-bool values, or absent meta.
    #[must_use]
    pub fn meta_capability(&self, key: &str) -> bool {
        self.agent
            .meta
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::{
        AgentCapabilities, InitializeResponse, ModelId, NewSessionResponse, ProtocolVersion,
        SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionId, SessionMode,
        SessionModeId, SessionModeState, SessionModelState,
    };

    use crate::spur_agent_caps::SpurAgentCaps;
    use crate::types::AgentKind;

    fn empty_init_response() -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::LATEST)
    }

    fn empty_new_session_response() -> NewSessionResponse {
        NewSessionResponse::new(SessionId::new("test-empty"))
    }

    #[test]
    fn serialized_caps_include_agent_kind() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        let value = serde_json::to_value(caps).expect("caps must serialize");

        assert_eq!(
            value.get("agent_kind").and_then(serde_json::Value::as_str),
            Some("codex-acp"),
            "caps snapshots must carry the agent kind that created them"
        );
    }

    fn agent_caps_with_meta(key: &str, val: serde_json::Value) -> AgentCapabilities {
        let mut meta = serde_json::Map::new();
        meta.insert(key.to_string(), val);
        AgentCapabilities::new().meta(meta)
    }

    #[test]
    fn current_model_label_resolves_via_available_models() {
        use agent_client_protocol::schema::ModelInfo;

        let init = empty_init_response();
        let new =
            NewSessionResponse::new(SessionId::new("model-label")).models(SessionModelState::new(
                ModelId::new("gpt-5"),
                vec![ModelInfo::new(ModelId::new("gpt-5"), "GPT-5")],
            ));
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("GPT-5"));
    }

    #[test]
    fn current_model_label_falls_back_to_raw_id() {
        let init = empty_init_response();
        let new = NewSessionResponse::new(SessionId::new("model-label"))
            .models(SessionModelState::new(ModelId::new("gpt-5"), vec![]));
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("gpt-5"));
    }

    #[test]
    fn current_model_label_returns_none_when_models_none() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert_eq!(caps.current_model_label(), None);
    }

    #[test]
    fn current_effort_label_resolves_via_select() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort"),
            "Reasoning effort",
            "medium",
            vec![SessionConfigSelectOption::new("medium", "Medium")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_effort_label().as_deref(), Some("Medium"));
    }

    #[test]
    fn model_label_from_config_options_returns_display_name_and_falls_back() {
        let named = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "sonnet",
            vec![
                SessionConfigSelectOption::new("sonnet", "Sonnet"),
                SessionConfigSelectOption::new("opus", "Opus"),
            ],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&named),
            Some("Sonnet")
        );

        let fallback = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "sonnet",
            vec![SessionConfigSelectOption::new("opus", "Opus")],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&fallback),
            Some("sonnet")
        );

        let no_model = vec![SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort"),
            "Reasoning effort",
            "medium",
            vec![SessionConfigSelectOption::new("medium", "Medium")],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&no_model),
            None
        );
    }

    #[test]
    fn usage_supported_delegates_to_quirks() {
        let init = empty_init_response();
        let new = empty_new_session_response();

        let claude = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);
        let codex = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(!claude.usage_supported());
        assert!(codex.usage_supported());
    }

    #[test]
    fn empty_responses_yield_all_false() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(!caps.supports_set_mode(), "empty modes => no set_mode");
        assert!(!caps.supports_set_model(), "empty models => no set_model");
        assert!(
            !caps.supports_set_config_option(),
            "empty config_options => no set_config_option"
        );
        assert!(
            !caps.supports_load_session(),
            "default agent_capabilities => no load_session"
        );
        assert!(
            !caps.meta_capability("terminal_output"),
            "absent meta => terminal_output false"
        );
    }

    #[test]
    fn codex_fixture_yields_all_set_caps_true() {
        let json = include_str!("../tests/data/codex_acp_0_12_new_session_response.json");
        let new: NewSessionResponse =
            serde_json::from_str(json).expect("codex fixture must deserialize");

        // Pair with a default InitializeResponse — set_* gating derives from
        // new_session state, not from AgentCapabilities flags.
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(
            caps.supports_set_mode(),
            "codex fixture has 3 modes => set_mode"
        );
        assert!(
            caps.supports_set_model(),
            "codex fixture has Some(models) => set_model"
        );
        assert!(
            caps.supports_set_config_option(),
            "codex fixture has 3 config_options => set_config_option"
        );
        assert_eq!(caps.config_options.len(), 3);
    }

    #[test]
    fn gemini_style_models_some_config_options_none() {
        use agent_client_protocol::schema::ModelInfo;
        // Synthetic gemini-style: models populated, config_options None/absent.
        let new =
            NewSessionResponse::new(SessionId::new("test-gemini")).models(SessionModelState::new(
                ModelId::new("gemini-1.5-pro"),
                vec![ModelInfo::new(
                    ModelId::new("gemini-1.5-pro"),
                    "Gemini 1.5 Pro",
                )],
            ));
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_model(), "gemini-style has models");
        assert!(
            !caps.supports_set_config_option(),
            "gemini-style has no config_options"
        );
        assert!(!caps.supports_set_mode(), "gemini-style has no modes");
    }

    #[test]
    fn models_present_but_empty_yields_false() {
        // Mirrors `supports_set_mode`'s `has_choices()` semantic: a
        // `Some(state)` with zero available models is not a usable
        // model-switch surface. The agent advertised the field but
        // exposed no choices, so `/model` should be hidden / disabled.
        let new = NewSessionResponse::new(SessionId::new("test-empty-models"))
            .models(SessionModelState::new(ModelId::new("only-current"), vec![]));
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.supports_set_model(),
            "Some(models) with empty available_models => not usable"
        );
    }

    #[test]
    fn models_with_available_yield_true() {
        use agent_client_protocol::schema::ModelInfo;
        let modes_state = SessionModelState::new(
            ModelId::new("default"),
            vec![ModelInfo::new(ModelId::new("default"), "Default")],
        );
        let new = NewSessionResponse::new(SessionId::new("test-models")).models(modes_state);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_model());
    }

    #[test]
    fn modes_present_but_empty_yields_false() {
        let modes = SessionModeState::new(SessionModeId::new("only-id"), vec![]);
        let new = NewSessionResponse::new(SessionId::new("test-empty-modes")).modes(modes);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.supports_set_mode(),
            "Some(modes) with empty available_modes => not usable"
        );
    }

    #[test]
    fn modes_with_available_yield_true() {
        let modes = SessionModeState::new(
            SessionModeId::new("default"),
            vec![SessionMode::new(SessionModeId::new("default"), "Default")],
        );
        let new = NewSessionResponse::new(SessionId::new("test-modes")).modes(modes);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_mode());
    }

    #[test]
    fn load_session_capability_propagates_from_agent_capabilities() {
        let mut init = empty_init_response();
        init.agent_capabilities = AgentCapabilities::new().load_session(true);
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_load_session());
    }

    #[test]
    fn meta_capability_reads_terminal_output_true() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("terminal_output", serde_json::Value::Bool(true));
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.meta_capability("terminal_output"));
        assert!(
            !caps.meta_capability("missing_key"),
            "missing keys read as false"
        );
    }

    #[test]
    fn meta_capability_non_bool_value_is_false() {
        let mut init = empty_init_response();
        init.agent_capabilities = agent_caps_with_meta(
            "terminal_output",
            serde_json::Value::String("true".to_string()),
        );
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.meta_capability("terminal_output"),
            "non-bool meta value is treated as false"
        );
    }
}
