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

// ─── Agent Status (during session) ─────────────────────────────────────

/// Status updates streamed from an agent during a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Thinking,
    Working,
    Idle,
    Done,
    Error,
}

// ─── Session Events ────────────────────────────────────────────────────

/// Events streamed back from an agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    /// Incremental text output from the agent.
    TextDelta(String),

    /// Agent started a tool call.
    ToolCallStart {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// Agent received a tool call result.
    ToolCallResult {
        id: String,
        output: serde_json::Value,
    },

    /// Agent status update (thinking, working, done, etc.).
    StatusUpdate(AgentStatus),

    /// Agent hit a rate limit.
    RateLimitHit {
        retry_after: Option<Duration>,
    },

    /// Agent reported an error.
    Error {
        code: i32,
        message: String,
    },

    /// Session completed.
    Complete {
        session_id: SessionId,
    },
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
}

// ─── Orchestrator Events ───────────────────────────────────────────────

/// Events emitted by the orchestrator for TUI/cost-tracker consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEvent {
    // Lifecycle
    BrainSpawned {
        agent: String,
        session: SessionId,
    },
    WorkerSpawned {
        agent: String,
        session: SessionId,
        worktree: PathBuf,
    },
    SessionCompleted {
        session: SessionId,
        success: bool,
    },

    // Streaming
    AgentOutput {
        session: SessionId,
        event: SessionEvent,
    },

    // Orchestration
    DelegationRequested {
        from: SessionId,
        to_agent: String,
        task: String,
    },
    DelegationCompleted {
        worker_session: SessionId,
        status: DelegationStatus,
    },
    ConflictDetected {
        files: Vec<PathBuf>,
    },

    // Rate limits
    RateLimitDetected {
        agent: String,
        retry_after: Option<Duration>,
    },
    BrainFailover {
        from: String,
        to: String,
    },

    // Cost
    CostUpdate {
        session: SessionId,
        agent: String,
        estimated_cost_usd: f64,
    },

    // PM
    IssueReceived {
        source: String,
        id: String,
    },
    PrCreated {
        url: String,
    },
    IssueUpdated {
        source: String,
        id: String,
        status: String,
    },
}

/// Result status of a delegation to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DelegationStatus {
    Success,
    Failed { error: String },
    Conflict { files: Vec<PathBuf> },
    Timeout,
}

/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    /// Git diff of worker's changes.
    pub diff: Option<String>,
    /// Summary of what the worker did.
    pub summary: Option<String>,
    /// Estimated cost in USD.
    pub estimated_cost_usd: f64,
}

