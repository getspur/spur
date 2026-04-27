//! `SpurAgentCaps` — frozen-per-session capability cache.
//!
//! See `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md` §6.1.
//!
//! Wraps two wire facts: the agent's `AgentCapabilities` (from
//! `InitializeResponse`) and the `NewSessionResponse` payload's
//! `modes` / `models` / `config_options`. Spur derives `set_*` support
//! from session-create state because ACP 0.12 does not gate these
//! protocol-stable methods on `AgentCapabilities` flags.

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::{
        AgentCapabilities, InitializeResponse, ModelId, NewSessionResponse, ProtocolVersion,
        SessionId, SessionMode, SessionModeId, SessionModeState, SessionModelState,
    };

    use crate::spur_agent_caps::SpurAgentCaps;

    fn empty_init_response() -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::LATEST)
    }

    fn empty_new_session_response() -> NewSessionResponse {
        NewSessionResponse::new(SessionId::new("test-empty"))
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
        let caps = SpurAgentCaps::new(&init, &new);

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
        let caps = SpurAgentCaps::new(&init, &new);

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
        // Synthetic gemini-style: models populated, config_options None/absent.
        let new = NewSessionResponse::new(SessionId::new("test-gemini")).models(
            SessionModelState::new(ModelId::new("gemini-1.5-pro"), vec![]),
        );
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new);

        assert!(caps.supports_set_model(), "gemini-style has models");
        assert!(
            !caps.supports_set_config_option(),
            "gemini-style has no config_options"
        );
        assert!(!caps.supports_set_mode(), "gemini-style has no modes");
    }

    #[test]
    fn modes_present_but_empty_yields_false() {
        let modes = SessionModeState::new(SessionModeId::new("only-id"), vec![]);
        let new = NewSessionResponse::new(SessionId::new("test-empty-modes")).modes(modes);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new);

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
        let caps = SpurAgentCaps::new(&init, &new);

        assert!(caps.supports_set_mode());
    }

    #[test]
    fn load_session_capability_propagates_from_agent_capabilities() {
        let mut init = empty_init_response();
        init.agent_capabilities = AgentCapabilities::new().load_session(true);
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new);

        assert!(caps.supports_load_session());
    }

    #[test]
    fn meta_capability_reads_terminal_output_true() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("terminal_output", serde_json::Value::Bool(true));
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new);

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
        let caps = SpurAgentCaps::new(&init, &new);

        assert!(
            !caps.meta_capability("terminal_output"),
            "non-bool meta value is treated as false"
        );
    }
}
