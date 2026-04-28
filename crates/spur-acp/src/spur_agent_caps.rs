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
    AgentCapabilities, InitializeResponse, NewSessionResponse, SessionConfigOption,
    SessionModeState, SessionModelState,
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
        SessionId, SessionMode, SessionModeId, SessionModeState, SessionModelState,
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
