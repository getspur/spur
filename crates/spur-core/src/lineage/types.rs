use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use spur_acp::SessionId;

pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};

/// Stable identifier for a logical executor. Survives retries (retries produce
/// a new `Attempt` under the same `ExecutorId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutorId(pub String);

impl ExecutorId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub kind: ReviewKind,       // from spur_acp
    pub payload: ReviewPayload, // from spur_acp
    pub requested_at: SystemTime,
    /// Carried from the event; used by the dispatcher to reject stale
    /// decisions targeting a superseded attempt.
    pub attempt_n: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub session_id: SessionId,
    // SystemTime (not Instant) so the projection is serde-serializable for
    // SessionHistory replay. Events crossing process boundaries require this.
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub status: AttemptStatus,
    pub cost_usd: f64,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorNode {
    pub id: ExecutorId,
    pub parent_id: Option<ExecutorId>,
    pub child_ids: Vec<ExecutorId>,
    pub agent: String,
    pub role: Role,
    pub task_spec: String,
    pub phase: LifecycleState,
    pub attempts: Vec<Attempt>,
    pub pending_review: Option<ReviewRequest>,

    /// Last time any event updated this executor. Used by the card's
    /// stale-color rule. `None` until first event other than spawn.
    /// Stored as SystemTime for serde-friendliness (projection is replay-pure;
    /// we bump this from event.occurred_at, NOT from SystemTime::now()).
    pub last_event_at: Option<SystemTime>,
    // TODO(close-feedback-loop-followup): wire AgentNotification tool
    // calls per-executor so these fields populate. Currently scope-
    // limited to defaults — InlineExecutorCard renders "0 calls · last:
    // (none)" for all executors.
    /// Incremented on each tool call observed for this executor.
    /// Scope-limited: always 0 in v1.
    pub tool_call_count: usize,
    /// Most recent tool call name, if any. Scope-limited: always None in v1.
    pub latest_tool_call: Option<String>,
    /// Files changed in the latest `Artifact::Diff` observed for this
    /// executor (snapshot of the most recent diff, not a cumulative
    /// total). `0` until the first Diff artifact arrives.
    pub files_touched_count: usize,
    /// Cached `DiffSummary` from the latest `Artifact::Diff` observed.
    pub latest_diff_summary: Option<DiffSummary>,
    /// Raw unified-diff text (for pager view). Populated alongside
    /// `latest_diff_summary` when `Artifact::Diff` carries `text: Some`.
    /// `None` means either no diff yet, or the emitter didn't carry text.
    pub latest_diff_text: Option<String>,
    /// Most recent error message, if any. Derived from the current
    /// attempt's error field.
    pub last_error: Option<String>,
}

impl ExecutorNode {
    /// The currently-active attempt (last element of `attempts`), if any.
    pub fn current_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    pub fn current_attempt_mut(&mut self) -> Option<&mut Attempt> {
        self.attempts.last_mut()
    }

    /// Seconds since this executor was spawned. Derives from the first
    /// attempt's started_at. Safe to call from render (not replay).
    pub fn elapsed_secs(&self) -> u64 {
        self.attempts
            .first()
            .and_then(|a| a.started_at.elapsed().ok().map(|d| d.as_secs()))
            .unwrap_or(0)
    }

    /// Seconds since the last event updated this executor. `None` if no
    /// non-spawn event has arrived yet. Safe to call from render.
    pub fn seconds_since_last_event(&self) -> Option<u64> {
        self.last_event_at
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
    }

    /// `(insertions, deletions)` from the latest diff summary, or `(0, 0)`.
    pub fn diff_totals(&self) -> (usize, usize) {
        self.latest_diff_summary
            .as_ref()
            .map(|d| (d.insertions, d.deletions))
            .unwrap_or((0, 0))
    }
}
