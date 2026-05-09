use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

// ─── Session & Agent Identifiers ───────────────────────────────────────

/// Unique identifier for an ACP session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype wrapping the brain's ACP session id. Distinct from worker
/// `SessionId`s by type — no `Default` impl, no `::new()` that takes
/// zero args, forcing every construction to carry a valid inner value.
/// Enforces INV-2: every `DelegationRequest` must carry a real brain
/// session id, not a `SessionId::new()` default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BrainSessionId(SessionId);

impl BrainSessionId {
    pub fn new(id: SessionId) -> Self {
        Self(id)
    }

    pub fn as_session_id(&self) -> &SessionId {
        &self.0
    }

    pub fn into_session_id(self) -> SessionId {
        self.0
    }
}

impl fmt::Display for BrainSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<SessionId> for BrainSessionId {
    fn from(id: SessionId) -> Self {
        Self(id)
    }
}

// ─── Agent Health ──────────────────────────────────────────────────────

/// Health status of a registered agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentHealth {
    /// Agent binary found and responds to initialize.
    Ready,
    /// Agent binary found but not yet checked.
    Unknown,
    /// Agent is currently executing a session.
    Busy,
    /// Agent binary not found or failed health check.
    Error(String),
    /// Agent is rate-limited, with optional recovery time.
    RateLimited { retry_after: Option<Duration> },
}

// ─── Agent Capabilities ────────────────────────────────────────────────

/// Capabilities reported by an agent during ACP initialize.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Human-readable agent name.
    pub name: Option<String>,
    /// Agent version string.
    pub version: Option<String>,
    /// Whether the agent supports MCP tool passthrough.
    pub supports_mcp: bool,
    /// Whether the agent supports session persistence.
    pub supports_sessions: bool,
    /// Whether the agent supports streaming responses.
    pub supports_streaming: bool,
    /// Raw capabilities object from ACP initialize response.
    pub raw: serde_json::Value,
}

// ─── Prompt Blocks ─────────────────────────────────────────────────────

/// A block in a prompt sent to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PromptBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

// ─── Cost Tier ─────────────────────────────────────────────────────────

/// Cost tier for an agent, used for cost estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostTier {
    High,
    Medium,
    Low,
}

// ─── Agent Role ────────────────────────────────────────────────────────

/// Whether an agent can serve as brain, worker, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Brain,
    Worker,
    Both,
}

impl AgentRole {
    /// True if this role can receive delegation tasks.
    pub fn is_worker_capable(&self) -> bool {
        matches!(self, AgentRole::Worker | AgentRole::Both)
    }
}

// ─── Transport Kind ────────────────────────────────────────────────────

/// Which transport implementation to use for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    Acp,
    Stdio,
    CliWrap,
    StreamJson,
}

// ─── Agent Kind ────────────────────────────────────────────────────────

/// Identifies the agent's wire-level idiom for the adapter layer.
///
/// Orthogonal to `TransportKind`: multiple kinds share the same transport
/// (e.g. `ClaudeCodeAcp`, `CodexAcp`, and `Kiro` all use `TransportKind::Acp`
/// via `NativeAcpConnection`). The adapter module reads `AgentKind` to pick
/// per-agent classifiers, observe-payload extractors, and mode-badge tables.
///
/// Unknown agents default to `Generic`, which applies only heuristic
/// fallbacks. Explicit TOML is preferred over inference — see
/// `docs/spur/agent-onboarding-cookbook.md` for the mapping table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Claude Code via `claude -p --output-format stream-json`.
    ClaudeStreamJson,
    /// Claude Code via `@agentclientprotocol/claude-agent-acp`.
    ClaudeCodeAcp,
    /// Codex via `@zed-industries/codex-acp` (npx or native binary).
    CodexAcp,
    /// Kiro CLI via `kiro-cli acp`.
    Kiro,
    /// Kimi Code CLI via `kimi acp`.
    Kimi,
    /// Any ACP-speaking agent not otherwise recognized.
    #[default]
    Generic,
}

impl AgentKind {
    /// Parse an agent identifier string (TOML name, display label, or
    /// serde kebab-case form) into an `AgentKind`. Unknown inputs return
    /// `AgentKind::Generic`.
    ///
    /// Used by the TUI to style per-executor traces and session panes
    /// when only the `ExecutorNode.agent: String` is in hand.
    pub fn from_name(name: &str) -> AgentKind {
        let norm = name.trim().to_ascii_lowercase();
        match norm.as_str() {
            "claude-stream-json" => AgentKind::ClaudeStreamJson,
            "claude-code-acp" | "claude" | "claude code" | "claude-code" => {
                AgentKind::ClaudeCodeAcp
            }
            "codex-acp" | "codex" => AgentKind::CodexAcp,
            "kiro" => AgentKind::Kiro,
            "kimi" | "kimi-code" | "kimi code" => AgentKind::Kimi,
            _ => AgentKind::Generic,
        }
    }
}

// ─── Cancel Mode ───────────────────────────────────────────────────────

/// How `AgentConnection::cancel` behaves for a given transport.
///
/// `AcpSoft` is a true ACP `session/cancel` notification — the agent
/// continues to exist and the session remains addressable.
///
/// `ProcessKill` signals the subprocess on cancel (SIGTERM for `Stdio`;
/// SIGKILL via `child.kill()` for `CliWrap`/`StreamJson`). The session
/// cannot be resumed without respawning — even if the `Stdio` agent
/// declines to exit on SIGTERM, the transport no longer treats the
/// session as usable.
///
/// Used by the TUI to show transport-aware cancel feedback. See
/// `docs/superpowers/specs/2026-04-14-session-detail-esc-cancel-design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelMode {
    /// ACP `session/cancel` notification; process stays alive.
    AcpSoft,
    /// Transport signals the subprocess on cancel; the session is dropped.
    ProcessKill,
}

// ─── Permission Flow ──────────────────────────────────────────────────

/// A permission request sent from the ACP thread to the TUI.
pub struct PermissionRequest {
    pub args: agent_client_protocol::schema::RequestPermissionRequest,
    pub reply_tx: tokio::sync::oneshot::Sender<PermissionResponse>,
}

/// The user's permission decision.
#[derive(Debug, Clone)]
pub struct PermissionResponse {
    pub option_id: String,
}

#[cfg(test)]
mod cancel_mode_tests {
    use super::CancelMode;

    #[test]
    fn cancel_mode_is_copy_and_equatable() {
        let a = CancelMode::AcpSoft;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(CancelMode::AcpSoft, CancelMode::ProcessKill);
    }
}

#[cfg(test)]
mod agent_kind_tests {
    use super::AgentKind;

    #[test]
    fn from_name_matches_kebab_case_serde_repr() {
        assert_eq!(
            AgentKind::from_name("claude-stream-json"),
            AgentKind::ClaudeStreamJson
        );
        assert_eq!(
            AgentKind::from_name("claude-code-acp"),
            AgentKind::ClaudeCodeAcp
        );
        assert_eq!(AgentKind::from_name("codex-acp"), AgentKind::CodexAcp);
        assert_eq!(AgentKind::from_name("kiro"), AgentKind::Kiro);
        assert_eq!(AgentKind::from_name("kimi"), AgentKind::Kimi);
        assert_eq!(AgentKind::from_name("generic"), AgentKind::Generic);
    }

    #[test]
    fn from_name_accepts_human_aliases() {
        assert_eq!(AgentKind::from_name("claude"), AgentKind::ClaudeCodeAcp);
        assert_eq!(
            AgentKind::from_name("Claude Code"),
            AgentKind::ClaudeCodeAcp
        );
        assert_eq!(AgentKind::from_name("codex"), AgentKind::CodexAcp);
    }

    #[test]
    fn from_name_unknown_defaults_to_generic() {
        assert_eq!(AgentKind::from_name("ollama-wizard"), AgentKind::Generic);
        assert_eq!(AgentKind::from_name(""), AgentKind::Generic);
    }
}
