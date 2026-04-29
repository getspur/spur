use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::SystemTime;

use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{MessageKind, PeerMessageId};
use spur_acp::SessionId;

pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};

/// Kind of entry in a worker's live stream buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStreamKind {
    Thought,
    Message,
    ToolCall,
}

/// A single entry in a worker's live stream buffer, derived from
/// `WorkerNotification` events. Stored on `ExecutorNode::stream_buffer`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStreamEntry {
    pub kind: WorkerStreamKind,
    pub text: String,
    pub occurred_at: SystemTime,
}

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
pub struct PeerEdge {
    pub message_id: PeerMessageId,
    pub source_delegation_id: DelegationId,
    pub target_delegation_id: DelegationId,
    pub kind: MessageKind,
    pub state: PeerEdgeState,
    pub injected_chars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerEdgeState {
    Accepted,
    Delivered,
    Consumed,
    Ignored,
    Expired,
    Dropped,
    Undeliverable,
    Rejected,
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

impl Attempt {
    /// Elapsed time on this attempt as of `now`. If `ended_at` is set
    /// (terminal phase observed), the result freezes at `ended_at - started_at`
    /// regardless of `now`. If `now` is somehow earlier than `started_at`
    /// (clock skew), returns `Duration::ZERO` rather than panicking.
    ///
    /// `now` is injected so callers in tests can supply a fixed clock for
    /// deterministic snapshot output.
    pub fn elapsed_at(&self, now: std::time::SystemTime) -> std::time::Duration {
        let end = self.ended_at.unwrap_or(now);
        end.duration_since(self.started_at)
            .unwrap_or(std::time::Duration::ZERO)
    }
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
    /// Bounded ring buffer of live worker stream entries.
    ///
    /// Retained for serde backward compatibility with pre-unification
    /// `session_metadata.json` files. The lineage projection no longer
    /// writes to this from `WorkerNotification` events — the DetailPane
    /// Stream tab consumes `WorkerNotification`s directly via
    /// `spur-tui/src/worker_streams.rs` (see
    /// `docs/superpowers/architecture/stream-pipeline.md`). Still
    /// cleared defensively on `ExecutorRetryStarted`.
    #[serde(default)]
    pub stream_buffer: VecDeque<WorkerStreamEntry>,
    /// Issue ID linked to this executor via delegation (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// Delegation request id linked to this executor, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<DelegationId>,
    /// Outbound peer-message edges projected from worker peer events.
    #[serde(default)]
    pub peer_edges: Vec<PeerEdge>,
}

impl ExecutorNode {
    /// The currently-active attempt (last element of `attempts`), if any.
    pub fn current_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    pub fn current_attempt_mut(&mut self) -> Option<&mut Attempt> {
        self.attempts.last_mut()
    }

    /// Seconds elapsed since the executor's first spawn. Freezes at
    /// `current_attempt.ended_at - first_attempt.started_at` once the
    /// **node** itself reaches a terminal phase; otherwise ticks against
    /// wall-clock `now`.
    ///
    /// The freeze decision is made on `node.phase`, not on
    /// `first_attempt.ended_at`. A retried-and-running executor has
    /// `attempts[0].ended_at = Some(...)` (from the failed first attempt)
    /// but `node.phase = Running`, and must continue to tick.
    ///
    /// Safe to call from render (not replay — this consults
    /// `SystemTime::now()`).
    pub fn elapsed_secs(&self) -> u64 {
        let first = match self.attempts.first() {
            Some(a) => a,
            None => return 0,
        };
        let end = match self.phase {
            LifecycleState::Succeeded | LifecycleState::Failed | LifecycleState::Cancelled => self
                .current_attempt()
                .and_then(|a| a.ended_at)
                .unwrap_or_else(SystemTime::now),
            _ => SystemTime::now(),
        };
        end.duration_since(first.started_at)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs()
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

#[cfg(test)]
mod attempt_elapsed_tests {
    use super::*;
    use spur_acp::SessionId;
    use std::time::Duration;

    fn fixture(started: SystemTime, ended: Option<SystemTime>) -> Attempt {
        Attempt {
            session_id: SessionId("s".into()),
            started_at: started,
            ended_at: ended,
            status: AttemptStatus::Running,
            cost_usd: 0.0,
            artifacts: vec![],
            error: None,
        }
    }

    #[test]
    fn running_attempt_elapsed_uses_now() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(150);
        let a = fixture(started, None);
        assert_eq!(a.elapsed_at(now), Duration::from_secs(50));
    }

    #[test]
    fn finished_attempt_elapsed_uses_ended_at() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let ended = SystemTime::UNIX_EPOCH + Duration::from_secs(120);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(999);
        let a = fixture(started, Some(ended));
        // elapsed should freeze at ended − started, ignoring `now`.
        assert_eq!(a.elapsed_at(now), Duration::from_secs(20));
    }

    #[test]
    fn negative_skew_is_zero() {
        // If clocks skew so ended_at < started_at (rare), saturate to ZERO.
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let ended = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        let a = fixture(started, Some(ended));
        assert_eq!(a.elapsed_at(now), Duration::ZERO);
    }

    #[test]
    fn executor_node_elapsed_secs_freezes_when_first_attempt_ended() {
        // ExecutorNode::elapsed_secs derives from the FIRST attempt's started_at.
        // When that attempt has ended_at set, elapsed must freeze.
        let started = SystemTime::now() - Duration::from_secs(60);
        let ended = SystemTime::now() - Duration::from_secs(30);
        let attempt = Attempt {
            session_id: SessionId("s".into()),
            started_at: started,
            ended_at: Some(ended),
            status: AttemptStatus::Succeeded,
            cost_usd: 0.0,
            artifacts: vec![],
            error: None,
        };
        let node = ExecutorNode {
            id: ExecutorId::new("e"),
            parent_id: None,
            child_ids: vec![],
            agent: "a".into(),
            role: spur_acp::Role::Executor,
            task_spec: String::new(),
            phase: LifecycleState::Succeeded,
            attempts: vec![attempt],
            pending_review: None,
            last_event_at: None,
            tool_call_count: 0,
            latest_tool_call: None,
            files_touched_count: 0,
            latest_diff_summary: None,
            latest_diff_text: None,
            last_error: None,
            stream_buffer: VecDeque::new(),
            issue_id: None,
            delegation_id: None,
            peer_edges: vec![],
        };
        // ended − started = 30s. Frozen.
        assert_eq!(node.elapsed_secs(), 30);
    }

    #[test]
    fn executor_node_elapsed_secs_ticks_for_retried_running_executor() {
        // A retried-and-running executor has attempts[0].ended_at = Some
        // (from the failed first attempt) but node.phase = Running. The
        // elapsed display must keep ticking against wall-clock from the
        // first spawn; a frozen-at-attempts[0].ended_at value would lie
        // about a still-running worker.
        let now = SystemTime::now();
        let first_started = now - Duration::from_secs(60);
        let first_ended = now - Duration::from_secs(30); // Failed at -30s.
        let retry_started = now - Duration::from_secs(20); // Retry pushed at -20s.

        let attempt0 = Attempt {
            session_id: SessionId("s1".into()),
            started_at: first_started,
            ended_at: Some(first_ended),
            status: AttemptStatus::Failed,
            cost_usd: 0.0,
            artifacts: vec![],
            error: Some("transient".into()),
        };
        let attempt1 = Attempt {
            session_id: SessionId("s2".into()),
            started_at: retry_started,
            ended_at: None,
            status: AttemptStatus::Running,
            cost_usd: 0.0,
            artifacts: vec![],
            error: None,
        };
        let node = ExecutorNode {
            id: ExecutorId::new("e"),
            parent_id: None,
            child_ids: vec![],
            agent: "a".into(),
            role: spur_acp::Role::Executor,
            task_spec: String::new(),
            phase: LifecycleState::Running,
            attempts: vec![attempt0, attempt1],
            pending_review: None,
            last_event_at: None,
            tool_call_count: 0,
            latest_tool_call: None,
            files_touched_count: 0,
            latest_diff_summary: None,
            latest_diff_text: None,
            last_error: None,
            stream_buffer: VecDeque::new(),
            issue_id: None,
            delegation_id: None,
            peer_edges: vec![],
        };

        // Expected: elapsed ticks from first.started_at against now.
        // Should be ~60s (loose bounds for test jitter).
        let elapsed = node.elapsed_secs();
        assert!(
            (59..=61).contains(&elapsed),
            "retried-running executor must tick from first spawn, got {}s (expected 59..=61)",
            elapsed
        );

        // Sanity: must NOT freeze at the first attempt's ended_at − started_at = 30s.
        assert_ne!(
            elapsed, 30,
            "elapsed_secs froze at attempts[0].ended_at − attempts[0].started_at; \
             retried-running executor regressed"
        );
    }
}
