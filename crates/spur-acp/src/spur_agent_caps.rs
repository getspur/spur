//! `SpurAgentCaps` — frozen-per-session capability cache.
//!
//! See `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md` §6.1.
//!
//! Wraps two wire facts: the agent's `AgentCapabilities` (from
//! `InitializeResponse`) and the per-session response payload's
//! `modes` / `config_options`. Spur derives `set_*` support
//! from session state because ACP 0.12 does not gate these
//! protocol-stable methods on `AgentCapabilities` flags.
//!
//! Named `SpurAgentCaps` (not `SessionCapabilities`) to avoid collision
//! with the SDK's `SessionCapabilities` struct that lives on
//! `AgentCapabilities`.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, InitializeResponse, LoadSessionResponse, NewSessionResponse,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionModeState,
};
use serde::{Deserialize, Serialize};

use crate::types::AgentKind;

/// What the agent told spur during `initialize` + `session/new` or `session/load`.
/// Captured ONCE per session at create/load time and frozen for the
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
            config_options: new_session.config_options.clone().unwrap_or_default(),
            agent_kind,
        }
    }

    /// Build from `initialize` plus the per-session state returned by
    /// `session/load`.
    #[must_use]
    pub fn from_loaded(
        initialize: &InitializeResponse,
        load_session: &LoadSessionResponse,
        agent_kind: AgentKind,
    ) -> Self {
        Self {
            agent: initialize.agent_capabilities.clone(),
            modes: load_session.modes.clone(),
            config_options: load_session.config_options.clone().unwrap_or_default(),
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
    /// `model` config option. ACP 1.0 removed the dedicated model state and
    /// expresses model choice through `session/set_config_option`.
    #[must_use]
    pub fn supports_set_model(&self) -> bool {
        self.model_option().is_some_and(has_select_choices)
    }

    /// The config option that represents model selection, when advertised.
    ///
    /// ACP 1.1 adds semantic categories; prefer the first option categorized
    /// as `Model`, and retain the legacy `id == "model"` fallback only for
    /// agents that omit `category`.
    #[must_use]
    pub fn model_option(&self) -> Option<&SessionConfigOption> {
        model_option_from(&self.config_options)
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

    /// `session/resume` is announced by `AgentCapabilities.session_capabilities.resume`.
    #[must_use]
    pub fn supports_resume_session(&self) -> bool {
        self.agent.session_capabilities.resume.is_some()
    }

    /// `session/delete` is announced by `AgentCapabilities.session_capabilities.delete`.
    #[must_use]
    pub fn supports_delete_session(&self) -> bool {
        self.agent.session_capabilities.delete.is_some()
    }

    /// `session/list` is announced by `AgentCapabilities.session_capabilities.list`.
    #[must_use]
    pub fn supports_list_sessions(&self) -> bool {
        self.agent.session_capabilities.list.is_some()
    }

    /// `session/close` is announced by `AgentCapabilities.session_capabilities.close`.
    #[must_use]
    pub fn supports_close_session(&self) -> bool {
        self.agent.session_capabilities.close.is_some()
    }

    /// Display label for the active model.
    #[must_use]
    pub fn current_model_label(&self) -> Option<String> {
        Self::model_label_from_config_options(&self.config_options).map(str::to_owned)
    }

    /// Display label for the active model from a config-options snapshot.
    /// Callers with live session state should pass that fresh snapshot
    /// instead of the frozen caps copy captured at session init.
    #[must_use]
    pub fn model_label_from_config_options(options: &[SessionConfigOption]) -> Option<&str> {
        let option = model_option_from(options)?;

        let SessionConfigKind::Select(select) = &option.kind else {
            return None;
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
        let option = thought_level_option_from(options)?;

        let SessionConfigKind::Select(select) = &option.kind else {
            return None;
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

        Some(name.unwrap_or_else(|| current.to_owned()))
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
        self.meta_capability_opt("usage_updates")
            .unwrap_or_else(|| crate::agent_quirks::usage_emit_default(self.agent_kind))
    }

    /// Probe a vendor `_meta` extension key and preserve absent/non-bool state.
    #[must_use]
    pub fn meta_capability_opt(&self, key: &str) -> Option<bool> {
        self.agent
            .meta
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(serde_json::Value::as_bool)
    }

    /// Probe a vendor `_meta` extension key (e.g. `"terminal_output"`).
    /// Returns false for missing keys, non-bool values, or absent meta.
    #[must_use]
    pub fn meta_capability(&self, key: &str) -> bool {
        self.meta_capability_opt(key).unwrap_or(false)
    }
}

fn model_option_from(options: &[SessionConfigOption]) -> Option<&SessionConfigOption> {
    option_by_category_or_absent_id(options, KnownConfigCategory::Model, "model")
}

pub fn thought_level_option_from(options: &[SessionConfigOption]) -> Option<&SessionConfigOption> {
    option_by_category_or_absent_id(
        options,
        KnownConfigCategory::ThoughtLevel,
        "reasoning_effort",
    )
}

#[derive(Clone, Copy)]
enum KnownConfigCategory {
    Model,
    ThoughtLevel,
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
            KnownConfigCategory::ThoughtLevel,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        )
    )
}

fn has_select_choices(option: &SessionConfigOption) -> bool {
    matches!(
        &option.kind,
        SessionConfigKind::Select(select)
            if match &select.options {
                SessionConfigSelectOptions::Ungrouped(options) => !options.is_empty(),
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .any(|group| !group.options.is_empty()),
                _ => false,
            }
    )
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, NewSessionResponse, SessionCapabilities,
        SessionCloseCapabilities, SessionConfigId, SessionConfigOption,
        SessionConfigOptionCategory, SessionConfigSelectOption, SessionDeleteCapabilities,
        SessionId, SessionListCapabilities, SessionMode, SessionModeId, SessionModeState,
        SessionResumeCapabilities,
    };
    use agent_client_protocol::schema::ProtocolVersion;

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
    fn current_model_label_resolves_via_config_option_choices() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-label"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gpt-5",
            vec![SessionConfigSelectOption::new("gpt-5", "GPT-5")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("GPT-5"));
    }

    #[test]
    fn model_option_prefers_model_category_over_model_id_allowlist() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-category"));
        new.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("model"),
                "Legacy model",
                "legacy-model",
                vec![SessionConfigSelectOption::new(
                    "legacy-model",
                    "Legacy Model",
                )],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("vendor_model"),
                "Vendor model",
                "vendor-sonnet",
                vec![SessionConfigSelectOption::new(
                    "vendor-sonnet",
                    "Vendor Sonnet",
                )],
            )
            .category(SessionConfigOptionCategory::Model),
        ]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        let option = caps.model_option().expect("model option must resolve");
        assert_eq!(option.id.0.as_ref(), "vendor_model");
        assert!(caps.supports_set_model());
        assert_eq!(caps.current_model_label().as_deref(), Some("Vendor Sonnet"));
    }

    #[test]
    fn current_model_label_falls_back_to_raw_id() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-label"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gpt-5",
            vec![SessionConfigSelectOption::new("other", "Other")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("gpt-5"));
    }

    #[test]
    fn current_model_label_returns_none_without_model_option() {
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
    fn current_effort_label_prefers_thought_level_category() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("reasoning_effort"),
                "Reasoning effort",
                "low",
                vec![SessionConfigSelectOption::new("low", "Low")],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("thinking_level"),
                "Thinking level",
                "high",
                vec![SessionConfigSelectOption::new("high", "High")],
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
        ]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_effort_label().as_deref(), Some("High"));
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
    fn usage_supported_honors_meta_true_for_claude_code() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("usage_updates", serde_json::Value::Bool(true));
        let new = empty_new_session_response();

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);

        assert!(caps.usage_supported());
    }

    #[test]
    fn usage_supported_honors_meta_false_for_codex() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("usage_updates", serde_json::Value::Bool(false));
        let new = empty_new_session_response();

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(!caps.usage_supported());
    }

    #[test]
    fn usage_supported_ignores_non_bool_meta_and_falls_back_to_quirks() {
        let mut init = empty_init_response();
        init.agent_capabilities = agent_caps_with_meta(
            "usage_updates",
            serde_json::Value::String("false".to_string()),
        );
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
        assert!(
            !caps.supports_set_model(),
            "empty config options => no set_model"
        );
        assert!(
            !caps.supports_set_config_option(),
            "empty config_options => no set_config_option"
        );
        assert!(
            !caps.supports_load_session(),
            "default agent_capabilities => no load_session"
        );
        assert!(
            !caps.supports_resume_session(),
            "default session_capabilities => no session/resume"
        );
        assert!(
            !caps.supports_delete_session(),
            "default session_capabilities => no session/delete"
        );
        assert!(
            !caps.supports_list_sessions(),
            "default session_capabilities => no session/list"
        );
        assert!(
            !caps.supports_close_session(),
            "default session_capabilities => no session/close"
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
            "codex fixture has model config option => set_model"
        );
        assert!(
            caps.supports_set_config_option(),
            "codex fixture has 3 config_options => set_config_option"
        );
        assert_eq!(caps.config_options.len(), 3);
    }

    #[test]
    fn model_config_option_sets_model_support() {
        let mut new = NewSessionResponse::new(SessionId::new("test-model-config"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gemini-1.5-pro",
            vec![SessionConfigSelectOption::new(
                "gemini-1.5-pro",
                "Gemini 1.5 Pro",
            )],
        )]);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_model(), "model config option has choices");
        assert!(
            caps.supports_set_config_option(),
            "model config option is advertised"
        );
        assert!(!caps.supports_set_mode(), "model config has no modes");
    }

    #[test]
    fn model_config_present_but_empty_yields_false() {
        let mut new = NewSessionResponse::new(SessionId::new("test-empty-model-option"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "only-current",
            Vec::<SessionConfigSelectOption>::new(),
        )]);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.supports_set_model(),
            "model config option with empty choices => not usable"
        );
    }

    #[test]
    fn model_config_with_available_yields_true() {
        let mut new = NewSessionResponse::new(SessionId::new("test-models"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "default",
            vec![SessionConfigSelectOption::new("default", "Default")],
        )]);
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
    fn session_lifecycle_capabilities_propagate_from_agent_capabilities() {
        let mut init = empty_init_response();
        init.agent_capabilities = AgentCapabilities::new().session_capabilities(
            SessionCapabilities::new()
                .resume(SessionResumeCapabilities::new())
                .delete(SessionDeleteCapabilities::new())
                .list(SessionListCapabilities::new())
                .close(SessionCloseCapabilities::new()),
        );
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_resume_session());
        assert!(caps.supports_delete_session());
        assert!(caps.supports_list_sessions());
        assert!(caps.supports_close_session());
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
