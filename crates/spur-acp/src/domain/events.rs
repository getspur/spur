use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use crate::domain::delegation::DelegationStatus;
use crate::types::{CancelMode, SessionId};
use agent_client_protocol::{SessionInfo, SessionNotification};

/// Review kind for `ExecutorReviewRequested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

/// Whether a file was read or written.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum FileTouchKind {
    Read,
    Write,
}

/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
    /// Structured delegation reasoning the brain emitted for this call.
    /// See design spec section C.5.
    #[serde(default)]
    pub delegation_plan: Option<crate::domain::DelegationPlan>,
    /// `Some(false)` when `delegation_plan.chosen` doesn't match the
    /// dispatched agent (after `normalize_agent_name`). Never blocks
    /// dispatch; exposed for reviewer visibility.
    #[serde(default)]
    pub chosen_matches_dispatched: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Monotonic sequence number assigned by the orchestrator's emit
    /// funnel (S2). Direct constructors set this to 0; the funnel
    /// overwrites. Subscribers can detect gaps and order chronologically.
    ///
    /// `#[serde(default)]` so pre-S2 event logs (no `seq` field) deserialize
    /// with `seq = 0` — the same sentinel the funnel uses for un-stamped
    /// events. Keeps Phase S3 JSONL replay backward-compatible.
    #[serde(default)]
    pub seq: u64,
    pub body: SpurEventBody,
}

impl SpurEvent {
    /// Convenience constructor. Use at emission sites. Do NOT call inside
    /// `apply` / projection code — timestamps there must come from the
    /// arriving event.
    ///
    /// Note: `seq` defaults to 0; the orchestrator's emit funnel (S2)
    /// overwrites with a real monotonic value before broadcast.
    pub fn now(body: SpurEventBody) -> Self {
        Self {
            occurred_at: SystemTime::now(),
            seq: 0,
            body,
        }
    }
}

/// Result of attempting `session/load` on a brain connection. Returned
/// from `load_brain_session` so the caller can distinguish "state
/// actually came back" from "we silently created a fresh session."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadOutcome {
    /// `session/load` returned the prior session state.
    Restored,
    /// `session/load` failed (unsupported, or errored) and we started a
    /// new session. `reason` is the underlying error.
    FellBackToNew { reason: String },
}

/// Issue summary carried in SpurEvents for TUI display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummaryEvent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

/// Full issue detail carried in the `IssueDetailFetched` event.
/// Mirrors `spur_pm::Issue` without taking a direct dependency on spur-pm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueDetailEvent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Canonical durable plan state rendered by the plan inspector UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshot {
    pub plan_id: String,
    pub status: String,
    pub progress: String,
    pub next_action: String,
    pub ready_to_merge: bool,
    pub counts: PlanSnapshotCounts,
    pub tasks: Vec<PlanSnapshotTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanSnapshotCounts {
    pub pending: u32,
    pub ready: u32,
    pub dispatched: u32,
    pub awaiting_review: u32,
    pub approved: u32,
    pub rejected: u32,
    pub failed: u32,
    pub cancelled: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshotTask {
    pub task_id: String,
    pub task_name: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    pub status: String,
    pub attempt: u32,
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<DiffSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded_by: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_action: String,
}

/// Snapshot of licensing state mirrored into the ACP event bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseStateEvent {
    pub status: LicenseStatusEvent,
    pub subject_kind: LicenseSubjectKind,
    pub plan: LicensePlan,
    pub features: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub binding_mode: LicenseBindingMode,
    pub offline_ok: bool,
    pub status_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseStatusEvent {
    Inactive,
    Active,
    Degraded,
    Invalid,
    ConfigError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseSubjectKind {
    User,
    Organization,
    Ci,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseBindingMode {
    NodeLocked,
    FloatingCi,
    Organization,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicensePlan {
    Community,
    StarterLtd,
    BuilderLtd,
    FounderLtd,
    Pro,
    Team,
    Enterprise,
    Unknown,
}

/// Reason a pending system continuation was evicted without being delivered.
/// See async-continuation design spec §Failure Cases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuationDropReason {
    BrainDisconnected,
    SessionSwap,
    Shutdown,
}

/// Why a brain session was retired. Companion to [`SpurEventBody::BrainRetired`].
/// See docs/superpowers/specs/2026-04-19-clear-command-session-reset-design.md.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrainRetireReason {
    /// User invoked `/clear`. Spur-local meta-command.
    UserClear,
    /// Session swap via `ResumeSession` (user selected a different session).
    ResumeSwitch,
    /// Orchestrator shutting down.
    Shutdown,
}

/// The discriminated payload of a [`SpurEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SpurEventBody {
    /// The orchestrator has started a connect-only brain prewarm.
    ///
    /// No ACP session exists yet; this only covers `initialize()` and the
    /// transport/process setup needed to make the first prompt faster.
    BrainConnectStarted {
        brain: String,
    },
    /// Connect-only brain prewarm completed successfully.
    ///
    /// No ACP session exists yet. The next prompt may reuse the warmed
    /// connection and call `new_session()` lazily.
    BrainConnected {
        brain: String,
    },
    /// Connect-only brain prewarm failed before any ACP session was created.
    BrainConnectFailed {
        brain: String,
        reason: String,
    },
    BrainSpawned {
        agent: String,
        session: SessionId,
    },
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
        /// How `session/cancel` is implemented for this session's transport.
        /// The TUI uses this to render transport-aware cancel feedback.
        cancel_mode: CancelMode,
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
    AgentNotification {
        session: SessionId,
        notification: Box<SessionNotification>,
    },
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
        /// Brain session that issued the delegation. Stamped by the MCP
        /// server onto every `DelegationRequest` and threaded through the
        /// orchestrator to this emission site.
        from: SessionId,
        to_agent: String,
        task: String,
        /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced so
        /// the brain conversation can correlate with the spawned executor
        /// via `DelegationDispatched`.
        request_id: String,
        /// Optional structured plan the brain passed alongside the
        /// delegate_* call. See design spec section C.7.
        #[serde(default)]
        delegation_plan: Option<crate::domain::DelegationPlan>,
        /// Issue ID linked to this delegation (if any). Set when the
        /// brain tool call carried an `issue_id` field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue_id: Option<String>,
    },
    /// Emitted immediately after the orchestrator spawns an executor
    /// for a brain delegation. Lets the brain-side session_detail
    /// view correlate its `DelegationRequested` trace entry with the
    /// new executor node so an inline executor card can render.
    DelegationDispatched {
        /// Brain session that issued the delegation. Stamped by the MCP
        /// server onto every `DelegationRequest` and threaded through the
        /// orchestrator to this emission site.
        from: SessionId,
        /// Matches the `request_id` on `DelegationRequested` /
        /// `DelegationRequest.id` (UUID).
        request_id: String,
        /// The executor node now spawned for this delegation.
        executor_id: String,
    },
    DelegationCompleted {
        worker_session: SessionId,
        status: DelegationStatus,
    },
    ConflictDetected {
        files: Vec<PathBuf>,
    },
    RateLimitDetected {
        agent: String,
        retry_after: Option<Duration>,
    },
    BrainFailover {
        from: String,
        to: String,
    },
    CostUpdate {
        session: SessionId,
        agent: String,
        estimated_cost_usd: f64,
    },
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
    },
    /// Emitted once at session start with all tracked issues.
    IssuesLoaded {
        issues: Vec<IssueSummaryEvent>,
    },

    /// Response to a TUI request for full issue detail.
    /// Follows SessionsListed / IssuesLoaded precedent for request-response on broadcast.
    IssueDetailFetched {
        /// The ID that was requested — TUI checks against focused issue
        /// to discard stale responses from navigation races.
        requested_id: String,
        /// Full issue data from PmService.
        issue: IssueDetailEvent,
    },

    /// Feedback for a failed issue operation initiated from TUI.
    IssueCommandError {
        operation: String,
        error: String,
    },

    /// Graph health alert summary from bv (beads_viewer) analysis.
    /// Emitted at startup and after each delegation completion.
    GraphAlertsSummary {
        total: usize,
        critical: usize,
        warning: usize,
        /// Human-readable alert messages (top 5) for TUI activity log.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        details: Vec<String>,
    },

    /// Licensing state snapshot, emitted at startup and whenever the
    /// provider refreshes cached state.
    LicenseUpdated {
        state: LicenseStateEvent,
    },

    // ── Interactive loop events ──────────────────────────────────────
    TurnComplete {
        session: SessionId,
    },
    BrainError {
        session: SessionId,
        message: String,
    },
    /// Brain subprocess appears to have died; a reconnect attempt is
    /// starting. Emitted BEFORE `connect_brain` runs so the TUI can
    /// display a banner immediately (subprocess spawn takes >1s).
    BrainReconnecting {
        session: SessionId,
        brain_name: String,
        /// Human-readable reason (usually the RPC error that tripped
        /// the detector).
        reason: String,
    },
    /// Reconnect succeeded. `outcome` says whether session state was
    /// restored or we fell back to a fresh session.
    BrainReconnected {
        session: SessionId,
        brain_name: String,
        outcome: LoadOutcome,
    },
    /// Reconnect attempt failed OR the circuit breaker tripped. The
    /// brain stays unset and the user must take an explicit action to
    /// retry.
    BrainReconnectFailed {
        session: SessionId,
        brain_name: String,
        reason: String,
    },
    /// The agent subprocess reported that authentication is required
    /// (e.g. `authRequired` error code, "/login" prompt). The TUI renders
    /// this as a dismissable banner instructing the user to run
    /// `claude /login` externally.
    AuthRequired {
        session: SessionId,
        message: String,
    },
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
    SessionsListed {
        agent: String,
        sessions: Vec<SessionInfo>,
    },
    SessionsListError {
        message: String,
    },
    /// Replayed conversation history from disk (when agent doesn't support load_session).
    SessionHistory {
        session: SessionId,
        entries: Vec<HistoryEntry>,
    },

    // ── Worker _spur/* ExtNotification vocabulary (S5) ─────────────
    /// Worker emitted `_spur/heartbeat` — periodic alive signal.
    /// The TUI uses this to detect stalled workers.
    WorkerHeartbeat {
        brain_session_id: SessionId,
        executor_id: String,
        /// Wall-clock at the worker; informational only.
        worker_ts: Option<String>,
    },

    /// Worker emitted `_spur/progress_milestone` — named checkpoint.
    /// The TUI shows this in the executor card.
    WorkerProgress {
        brain_session_id: SessionId,
        executor_id: String,
        name: String,
        /// Optional 0..=100 percentage.
        pct: Option<u8>,
    },

    /// Live session notification from a running worker agent. Emitted
    /// by the orchestrator for every `SessionNotification` received
    /// from a worker's `drive_prompt_notifications` stream. The TUI
    /// lineage projection converts these into `WorkerStreamEntry`
    /// items on the executor's `stream_buffer` for the detail-pane
    /// Stream tab.
    WorkerNotification {
        brain_session_id: SessionId,
        executor_id: String,
        notification: Box<SessionNotification>,
    },

    /// Worker read or wrote a file. Either emitted explicitly by the
    /// worker via `_spur/file_touched`, or synthesized by the
    /// orchestrator from observed ToolCall events with a 200ms
    /// de-duplication window.
    WorkerFileTouched {
        brain_session_id: SessionId,
        executor_id: String,
        path: std::path::PathBuf,
        kind: FileTouchKind,
    },

    /// Durable beads-backed plan state for a session.
    PlanSnapshotUpdated {
        session_id: SessionId,
        snapshot: Box<PlanSnapshot>,
    },

    /// Brain submitted a review verdict on a plan task.
    PlanTaskReviewed {
        plan_id: String,
        task_id: String,
        /// Human-readable task name derived from task text (first line, 60
        /// chars). `None` on replay of pre-Phase-2 events.
        #[serde(default)]
        task_name: Option<String>,
        /// "approve" | "reject" | "request_changes"
        decision: String,
        feedback: Option<String>,
        attempt: u32,
        /// Attempt budget. Carried in the event so renderers don't need a
        /// cross-crate const import. Defaults to 0 on pre-Phase-2 replay.
        #[serde(default)]
        max_attempts: u32,
    },

    /// A plan task was re-dispatched for iteration (attempt > 1).
    PlanTaskIterating {
        plan_id: String,
        task_id: String,
        /// Human-readable task name. `None` on replay of pre-Phase-2 events.
        #[serde(default)]
        task_name: Option<String>,
        /// New attempt number (the attempt that just started, i.e., old_attempt + 1).
        attempt: u32,
        /// Attempt budget. Defaults to 0 on pre-Phase-2 replay.
        #[serde(default)]
        max_attempts: u32,
        delegation_id: String,
    },

    // ── Plan lifecycle events (INV-7) ─────────────────────────────────────────
    /// Emitted once when a submitted plan reaches a terminal state (no tasks
    /// left to dispatch). Counts reflect the final status of all tasks.
    /// Brain awaits this instead of polling get_plan_status.
    PlanCompleted {
        plan_id: String,
        approved: u32,
        rejected: u32,
        failed: u32,
        #[serde(default)]
        cancelled: u32,
    },
    /// Emitted when all tasks in a plan are Approved. Distinct from
    /// PlanCompleted (which fires on any terminal state). Brain treats this
    /// as the merge-authorization signal.
    PlanReadyToMerge {
        plan_id: String,
    },

    /// A pending system continuation was evicted without being delivered
    /// to the brain. See async-continuation design spec §Failure Cases.
    ContinuationDropped {
        delegation_id: String,
        reason: ContinuationDropReason,
    },

    /// Emitted immediately before the orchestrator calls
    /// `connection.prompt(...)` for a brain turn. Pairs with
    /// `DelegationCompleted` to make INV-C3 (UI-visible event precedes
    /// model-visible continuation) directly verifiable via `seq` ordering.
    ///
    /// `turn_kind` is one of `"user_only" | "merged" | "continuation_only"`;
    /// `continuations_count` is the number of `BrainContinuation`s
    /// materialized as self-describing `spur://continuation/{id}` blocks
    /// for this turn (0 for `user_only`).
    PromptDispatched {
        session: SessionId,
        turn_kind: String,
        continuations_count: usize,
    },

    /// Emitted by the orchestrator when a brain session is retired via
    /// `retire_active_brain` (e.g. `/clear`, `ResumeSession`, shutdown).
    ///
    /// The lineage projection folds this event by cascading the named
    /// brain and all non-terminal descendants to
    /// [`LifecycleState::Cancelled`], stamping
    /// `ended_at = event.occurred_at` on each current attempt and
    /// draining the cascaded ids from the pending-review queue.
    ///
    /// The orchestrator emits this **before** aborting the brain's
    /// background tasks so trailing notifications landing afterward
    /// project against the already-closed state deterministically.
    BrainRetired {
        session: SessionId,
        reason: BrainRetireReason,
    },
}

/// A single entry in a replayed conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: String,
    pub text: String,
}

#[cfg(test)]
mod reconnect_event_tests {
    use super::*;
    use crate::SessionId;

    #[test]
    fn brain_connect_events_construct() {
        let _ = SpurEventBody::BrainConnectStarted {
            brain: "kiro".into(),
        };
        let _ = SpurEventBody::BrainConnected {
            brain: "kiro".into(),
        };
        let _ = SpurEventBody::BrainConnectFailed {
            brain: "kiro".into(),
            reason: "initialize failed".into(),
        };
    }

    #[test]
    fn load_outcome_variants_construct() {
        let _ = LoadOutcome::Restored;
        let _ = LoadOutcome::FellBackToNew {
            reason: "session/load returned error".into(),
        };
    }

    #[test]
    fn brain_reconnect_events_construct() {
        let s = SessionId::new();
        let _ = SpurEventBody::BrainReconnecting {
            session: s.clone(),
            brain_name: "kiro".into(),
            reason: "ACP thread died during prompt".into(),
        };
        let _ = SpurEventBody::BrainReconnected {
            session: s.clone(),
            brain_name: "kiro".into(),
            outcome: LoadOutcome::Restored,
        };
        let _ = SpurEventBody::BrainReconnectFailed {
            session: s,
            brain_name: "kiro".into(),
            reason: "circuit breaker tripped".into(),
        };
    }
}

#[cfg(test)]
mod cancel_mode_field_tests {
    use super::{SpurEvent, SpurEventBody};
    use crate::{CancelMode, SessionId};

    #[test]
    fn agent_session_ready_carries_cancel_mode() {
        let ev = SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: SessionId("s".to_string()),
            acp_session_id: "acp-1".to_string(),
            brain: "kiro".to_string(),
            resumed: false,
            cancel_mode: CancelMode::AcpSoft,
        });
        match ev.body {
            SpurEventBody::AgentSessionReady { cancel_mode, .. } => {
                assert_eq!(cancel_mode, CancelMode::AcpSoft);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod review_payload_tests {
    use super::*;
    use crate::domain::DelegationPlan;

    #[test]
    fn review_payload_default_has_none_plan() {
        let p = ReviewPayload {
            summary: "s".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
        };
        assert!(p.delegation_plan.is_none());
        assert!(p.chosen_matches_dispatched.is_none());
    }

    #[test]
    fn review_payload_round_trips_with_plan() {
        let plan = DelegationPlan {
            chosen: Some("kiro".into()),
            rationale: Some("because".into()),
            ..Default::default()
        };
        let p = ReviewPayload {
            summary: "".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: Some(plan),
            chosen_matches_dispatched: Some(true),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ReviewPayload = serde_json::from_str(&json).unwrap();
        assert!(back.delegation_plan.is_some());
        assert_eq!(back.chosen_matches_dispatched, Some(true));
    }
}

#[cfg(test)]
mod delegation_requested_tests {
    use super::*;
    use crate::domain::DelegationPlan;
    use crate::SessionId;

    #[test]
    fn delegation_requested_event_carries_optional_plan() {
        let plan = DelegationPlan {
            chosen: Some("claude".into()),
            ..Default::default()
        };
        let body = SpurEventBody::DelegationRequested {
            from: SessionId::new(),
            to_agent: "claude".into(),
            task: "do things".into(),
            request_id: "req-1".into(),
            delegation_plan: Some(plan.clone()),
            issue_id: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"delegation_plan\""));
    }

    #[test]
    fn delegation_requested_event_roundtrips_without_plan() {
        let body = SpurEventBody::DelegationRequested {
            from: SessionId::new(),
            to_agent: "codex".into(),
            task: "tiny fix".into(),
            request_id: "req-2".into(),
            delegation_plan: None,
            issue_id: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        match back {
            SpurEventBody::DelegationRequested {
                delegation_plan, ..
            } => {
                assert!(delegation_plan.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }
}
