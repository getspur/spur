use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use agent_client_protocol::{SessionNotification, SessionInfo};
use crate::types::SessionId;
use crate::domain::delegation::DelegationStatus;

/// Review kind for `ExecutorReviewRequested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorReviewPayload {
    pub summary: String,
    pub diff_summary: Option<ExecutorDiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorDiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<PathBuf>,
}

/// Artifact kinds emitted by an executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorArtifactPayload {
    Diff(ExecutorDiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}

/// User's decision on a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}

/// Events emitted by the orchestrator for TUI/cost-tracker consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEvent {
    BrainSpawned { agent: String, session: SessionId },
    WorkerSpawned { agent: String, session: SessionId, worktree: PathBuf },
    SessionCompleted { session: SessionId, success: bool },
    AgentNotification { session: SessionId, notification: SessionNotification },
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
    // ── Executor lineage events ────────────────────────────────────
    ExecutorSpawned {
        id: String,
        parent_id: Option<String>,
        session_id: SessionId,
        agent: String,
        role: String,           // "Brain" | "Executor" | "SubExecutor"
        task_spec: String,
    },
    ExecutorPhaseChanged {
        id: String,
        phase: String,          // serialized `LifecycleState` variant name
    },
    ExecutorArtifact {
        id: String,
        artifact: ExecutorArtifactPayload,
    },
    ExecutorReviewRequested {
        id: String,
        kind: ExecutorReviewKind,
        payload: ExecutorReviewPayload,
        requested_at: SystemTime,
    },
    ExecutorReviewResolved {
        id: String,
        decision: ExecutorReviewDecision,
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
