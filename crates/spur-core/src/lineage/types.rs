use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

use spur_acp::SessionId;

/// Stable identifier for a logical executor. Survives retries (retries produce
/// a new `Attempt` under the same `ExecutorId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutorId(pub String);

impl ExecutorId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

pub use spur_acp::{LifecycleState, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub kind: ReviewKind,
    pub payload: ReviewPayload,
    pub requested_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    Diff(DiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
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
}

impl ExecutorNode {
    /// The currently-active attempt (last element of `attempts`), if any.
    pub fn current_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    pub fn current_attempt_mut(&mut self) -> Option<&mut Attempt> {
        self.attempts.last_mut()
    }
}
