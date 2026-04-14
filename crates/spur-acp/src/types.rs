use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
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

// ─── MCP Endpoint ──────────────────────────────────────────────────────

/// Endpoint info for SPUR's MCP callback server, passed to agents during ACP init.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpEndpoint {
    /// Path to Unix domain socket.
    pub socket_path: PathBuf,
    /// Human-readable server name.
    pub server_name: String,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CancelMode {
    /// ACP `session/cancel` notification; process stays alive.
    AcpSoft,
    /// Transport signals the subprocess on cancel; the session is dropped.
    ProcessKill,
}

// ─── Permission Flow ──────────────────────────────────────────────────

/// A permission request sent from the ACP thread to the TUI.
pub struct PermissionRequest {
    pub args: agent_client_protocol::RequestPermissionRequest,
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

