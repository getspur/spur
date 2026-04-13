use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use agent_client_protocol::{SessionNotification, SessionInfo};
use crate::types::SessionId;
use crate::domain::delegation::DelegationStatus;

/// Review kind for `ExecutorReviewRequested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<PathBuf>,
}

/// Artifact kinds emitted by an executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    Diff(DiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}

/// User's decision on a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    Spawning,
    Running,
    AwaitingReview,
    Resuming,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Brain,
    Executor,
    SubExecutor,
}

/// Envelope wrapping every domain event with an occurrence timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    pub body: SpurEventBody,
}

impl SpurEvent {
    /// Convenience constructor. Use at emission sites. Do NOT call inside
    /// `apply` / projection code — timestamps there must come from the
    /// arriving event.
    pub fn now(body: SpurEventBody) -> Self {
        Self { occurred_at: SystemTime::now(), body }
    }
}

/// The discriminated payload of a [`SpurEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEventBody {
    BrainSpawned { agent: String, session: SessionId },
    WorkerSpawned { agent: String, session: SessionId, worktree: PathBuf },
    SessionCompleted { session: SessionId, success: bool },
    AgentNotification { session: SessionId, notification: SessionNotification },
    /// Vendor-extension notification received from the agent side.
    /// Routing by `method` name is the receiver's responsibility.
    /// `method` is the wire form (e.g. `"_kiro.dev/commands/available"`),
    /// with the leading `_` preserved for reader convenience.
    AgentExtNotification {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    DelegationRequested { from: SessionId, to_agent: String, task: String },
    DelegationCompleted { worker_session: SessionId, status: DelegationStatus },
    ConflictDetected { files: Vec<PathBuf> },
    RateLimitDetected { agent: String, retry_after: Option<Duration> },
    BrainFailover { from: String, to: String },
    CostUpdate { session: SessionId, agent: String, estimated_cost_usd: f64 },
    IssueReceived { source: String, id: String },
    PrCreated { url: String },
    IssueUpdated { source: String, id: String, status: String },
    // ── Interactive loop events ──────────────────────────────────────
    TurnComplete { session: SessionId },
    BrainError { session: SessionId, message: String },
    /// The agent subprocess reported that authentication is required
    /// (e.g. `authRequired` error code, "/login" prompt). The TUI renders
    /// this as a dismissable banner instructing the user to run
    /// `claude /login` externally.
    AuthRequired { session: SessionId, message: String },
    // ── Executor lineage events ────────────────────────────────────
    ExecutorSpawned {
        id: String,
        parent_id: Option<String>,
        session_id: SessionId,
        agent: String,
        role: Role,
        task_spec: String,
    },
    ExecutorPhaseChanged {
        id: String,
        phase: LifecycleState,
    },
    ExecutorArtifact {
        id: String,
        artifact: Artifact,
    },
    ExecutorReviewRequested {
        id: String,
        kind: ReviewKind,
        payload: ReviewPayload,
        // Note: requested_at removed — envelope `occurred_at` carries it now.
    },
    ExecutorReviewResolved {
        id: String,
        decision: ReviewDecision,
    },
    ExecutorRetryStarted {
        id: String,
        attempt_n: u32,
        reason: String,
        new_session_id: SessionId,
    },
    // ── Session picker events ───────────────────────────────────────
    SessionsListed { agent: String, sessions: Vec<SessionInfo> },
    SessionsListError { message: String },
    /// Replayed conversation history from disk (when agent doesn't support load_session).
    SessionHistory { session: SessionId, entries: Vec<HistoryEntry> },
}

/// A single entry in a replayed conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: String,
    pub text: String,
}
