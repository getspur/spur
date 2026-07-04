use crate::types::AgentKind;

/// How to select a named profile for a fresh session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectStrategy {
    /// `session/set_config_option` with this config id, value = profile name.
    ConfigOption { id: String },
    /// `session/set_mode` with modeId = profile name.
    SessionMode,
    /// No selection surface.
    None,
}

/// Per-kind profile wiring strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileStrategy {
    pub select: SelectStrategy,
    pub materialize: bool,
}

impl ProfileStrategy {
    pub fn for_kind(kind: AgentKind) -> Self {
        match kind {
            AgentKind::ClaudeCodeAcp => Self {
                select: SelectStrategy::ConfigOption { id: "agent".into() },
                materialize: true,
            },
            AgentKind::OpenCode => Self {
                select: SelectStrategy::ConfigOption { id: "mode".into() },
                materialize: true,
            },
            AgentKind::Kiro => Self {
                select: SelectStrategy::SessionMode,
                materialize: true,
            },
            AgentKind::CodexAcp | AgentKind::ClaudeStreamJson => Self {
                select: SelectStrategy::None,
                materialize: true,
            },
            AgentKind::Kimi | AgentKind::Gemini | AgentKind::Generic => Self {
                select: SelectStrategy::None,
                materialize: false,
            },
        }
    }

    pub fn resolve(kind: AgentKind, cfg: Option<&ProfileConfig>) -> Self {
        let default = Self::for_kind(kind);
        let Some(cfg) = cfg else {
            return default;
        };
        Self {
            select: cfg
                .select
                .as_deref()
                .and_then(parse_select_strategy)
                .unwrap_or(default.select),
            materialize: cfg.materialize.unwrap_or(default.materialize),
        }
    }
}

fn parse_select_strategy(raw: &str) -> Option<SelectStrategy> {
    let raw = raw.trim();
    if let Some(id) = raw.strip_prefix("config_option:") {
        if !id.is_empty() {
            return Some(SelectStrategy::ConfigOption { id: id.into() });
        }
    }
    match raw {
        "session_mode" => Some(SelectStrategy::SessionMode),
        "none" => Some(SelectStrategy::None),
        _ => None,
    }
}

/// `[agents.entries.profile]` override for per-kind profile wiring.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialize: Option<bool>,
}
