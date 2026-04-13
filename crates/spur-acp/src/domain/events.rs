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
    Diff {
        summary: DiffSummary,
        /// Raw unified-diff text retained for pager display.
        /// `None` means the emitter didn't have the text available
        /// (e.g., replay of a pre-Task-14 event, or synthetic artifact).
        text: Option<String>,
    },
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
    /// Emitted AFTER a brain session is established (fresh or resumed) and
    /// the agent-authoritative ACP session id is known. The TUI persists
    /// the (spur_id → acp_id, brain) mapping so the next `spur watch` run
    /// can resume by the real ACP id.
    ///
    /// - `session`: the SPUR session id (matches the earlier `BrainSpawned`).
    /// - `acp_session_id`: the id the agent assigned (stable across runs
    ///   where the agent supports `session/load`).
    /// - `brain`: the brain agent name that owns this ACP id.
    /// - `resumed`: `true` iff `session/load` succeeded. `false` when the
    ///   path fell back to `new_session` or spawned fresh.
    AgentSessionReady {
        session: SessionId,
        acp_session_id: String,
        brain: String,
        resumed: bool,
    },
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
    DelegationRequested {
        /// **Currently populated with the worker session**, not the brain session —
        /// pre-existing limitation: the brain session id is not threaded into
        /// the orchestrator. To be corrected alongside `DelegationDispatched.from`
        /// in the follow-up task that wires the brain session through.
        from: SessionId,
        to_agent: String,
        task: String,
        /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced so
        /// the brain conversation can correlate with the spawned executor
        /// via `DelegationDispatched`.
        request_id: String,
    },
    /// Emitted immediately after the orchestrator spawns an executor
    /// for a brain delegation. Lets the brain-side session_detail
    /// view correlate its `DelegationRequested` trace entry with the
    /// new executor node so an inline executor card can render.
    DelegationDispatched {
        /// **Currently populated with the worker session**, not the brain session —
        /// the brain session is not yet threaded into the orchestrator's delegation
        /// path. Will become the brain session once the brain-side session id is
        /// plumbed through `DelegationRequest` (follow-up task; see Task 4+ of the
        /// close-feedback-loop plan).
        from: SessionId,
        /// Matches the `request_id` on `DelegationRequested` /
        /// `DelegationRequest.id` (UUID).
        request_id: String,
        /// The executor node now spawned for this delegation.
        executor_id: String,
    },
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
        /// Which attempt this review gates. Propagated back via
        /// `UserInput::SubmitReview` for supersession guard.
        attempt_n: u32,
        kind: ReviewKind,
        payload: ReviewPayload,
    },
    ExecutorReviewResolved {
        id: String,
        decision: ReviewDecision,
    },
    /// The orchestrator abandoned a pending review (e.g., because the
    /// brain's tool call was cancelled). Emitted so the lineage
    /// projection records the abandonment rather than showing a silent
    /// disappearance.
    ExecutorReviewCancelled {
        id: String,
        reason: String,
    },
    ExecutorRetryStarted {
        id: String,
        /// 1-based index of the new attempt; validated against the projection's
        /// current attempt count to detect dropped retry events.
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
