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
        matches!(self, Self::Worker | Self::Both)
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
    /// Codex via `@agentclientprotocol/codex-acp@1.7.0` (npx or global install).
    CodexAcp,
    /// Kiro CLI via `kiro-cli acp`.
    Kiro,
    /// Kimi Code CLI via `kimi acp`.
    Kimi,
    /// Gemini CLI via `gemini --acp`.
    Gemini,
    /// `OpenCode` CLI via `opencode acp`.
    OpenCode,
    /// xAI Grok Build CLI via `grok agent stdio`.
    Grok,
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
    pub fn from_name(name: &str) -> Self {
        let norm = name.trim().to_ascii_lowercase();
        match norm.as_str() {
            "claude-stream-json" => Self::ClaudeStreamJson,
            "claude-code-acp" | "claude" | "claude code" | "claude-code" => Self::ClaudeCodeAcp,
            "codex-acp" | "codex" => Self::CodexAcp,
            "kiro" => Self::Kiro,
            "kimi" | "kimi-code" | "kimi code" => Self::Kimi,
            "gemini" | "gemini-acp" | "gemini-cli" | "gemini cli" => Self::Gemini,
            "opencode" | "open-code" => Self::OpenCode,
            "grok" | "grok-code" | "grok build" | "grok-build" => Self::Grok,
            _ => Self::Generic,
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

/// Shared lease identity stamped onto interactive permission requests.
///
/// The ACP permission handler snapshots generation + per-session fence **at
/// handler entry** so a late send after lease rotation still carries the
/// fence observed when the request began — not the live lease at send time.
///
/// Fences are session-keyed so concurrent sessions on one manager do not
/// clobber each other.
#[derive(Debug, Default)]
pub struct PermissionLeaseStamp {
    generation: std::sync::atomic::AtomicU64,
    fences: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl PermissionLeaseStamp {
    /// Allocate a new shared stamp (generation and fences both empty/zero).
    #[must_use]
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Publish the connection generation that currently owns the ACP process.
    pub fn set_generation(&self, generation: u64) {
        self.generation
            .store(generation, std::sync::atomic::Ordering::SeqCst);
    }

    /// Publish the live operation fence for one ACP session (`0` is unused).
    pub fn set_session_fence(&self, session_id: &str, fence: u64) {
        self.fences
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_owned(), fence);
    }

    /// Clear the live fence for one session (no permission owner for it).
    pub fn clear_session_fence(&self, session_id: &str) {
        self.fences
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    /// Snapshot `(generation, operation_fence)` for one session.
    #[must_use]
    pub fn snapshot_for_session(&self, session_id: &str) -> (u64, u64) {
        let generation = self.generation.load(std::sync::atomic::Ordering::SeqCst);
        let fence = self
            .fences
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .copied()
            .unwrap_or(0);
        (generation, fence)
    }
}

/// A permission request sent from the ACP thread to the interactive UI.
pub struct PermissionRequest {
    pub args: agent_client_protocol::schema::v1::RequestPermissionRequest,
    pub reply_tx: tokio::sync::oneshot::Sender<PermissionResponse>,
    /// Connection generation captured at permission-handler entry.
    pub generation: u64,
    /// Operation fence captured at permission-handler entry (`0` = no owner).
    pub operation_fence: u64,
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
    fn from_name_recognizes_grok_as_first_class_kind() {
        let kind = AgentKind::from_name("grok");
        assert_eq!(kind, AgentKind::Grok);
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"grok\"");
    }

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
        assert_eq!(AgentKind::from_name("gemini"), AgentKind::Gemini);
        assert_eq!(AgentKind::from_name("opencode"), AgentKind::OpenCode);
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
        assert_eq!(AgentKind::from_name("gemini-cli"), AgentKind::Gemini);
    }

    #[test]
    fn from_name_unknown_defaults_to_generic() {
        assert_eq!(AgentKind::from_name("ollama-wizard"), AgentKind::Generic);
        assert_eq!(AgentKind::from_name(""), AgentKind::Generic);
    }
}
