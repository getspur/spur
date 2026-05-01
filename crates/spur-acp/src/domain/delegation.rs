use crate::DiffSummary;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Typed identifier for a delegation request.
///
/// Wraps the UUID-v4 string that used to flow as a bare `String` through
/// `DelegationRequest.id`, `BrainContinuation.delegation_id`, and the
/// MCP-side `active_delegations` / `completed_delegations` maps. The
/// `serde(transparent)` attribute keeps the JSON wire format identical
/// to the pre-newtype representation (plain string) so no brain-side
/// migration is required. `From<String>` / `From<&str>` exist to ease
/// the gradual migration of call sites that still produce bare strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DelegationId(pub String);

impl DelegationId {
    /// Mint a fresh UUID-v4-backed id.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for DelegationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DelegationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for DelegationId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DelegationId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<DelegationId> for String {
    fn from(id: DelegationId) -> Self {
        id.0
    }
}

/// Ergonomic comparisons against bare string literals so assertions and
/// log-field checks keep working after the newtype migration.
impl PartialEq<str> for DelegationId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for DelegationId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for DelegationId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// Result status of a delegation to a worker.
///
/// `Rejected` is reserved for human-issued rejections arriving via the
/// review gate. System-applied timeouts use `TimedOut` so the brain can
/// distinguish actionable feedback from "nobody reviewed in time."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DelegationStatus {
    // Pre-existing worker-level variants.
    Success,
    Failed {
        error: String,
    },
    Conflict {
        files: Vec<PathBuf>,
    },
    /// Worker hung past the hard worker-hang deadline (distinct from
    /// review-gate timeout, which is `TimedOut`).
    Timeout,

    // Review-gate variants.
    /// Human reviewer rejected the work. `reason` is actionable feedback
    /// the brain can address on a retry.
    Rejected {
        reason: String,
    },
    /// Human reviewer approved-with-modifications; `reviewer_note` is a
    /// caveat the brain should consider alongside the accepted diff.
    Modified {
        reviewer_note: String,
    },
    /// Review timeout fired. `fallback` records the configured
    /// `TimeoutFallback` that was applied.
    TimedOut {
        #[serde(with = "duration_serde")]
        waited_for: Duration,
        fallback: TimeoutFallback,
    },
    /// INV-6: cancellation requested via `CancellationControl::cancel(id)`.
    /// `reason` describes who/why cancellation fired.
    Cancelled {
        reason: String,
    },
    /// Worker setup failed before the worker process could run.
    SetupFailed {
        error: AttemptSetupError,
    },
}

/// Structured setup-level failure for a delegation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AttemptSetupError {
    SnapshotFailed {
        error: String,
    },
    WorktreeFailed {
        error: String,
    },
    InitFailed {
        error: String,
    },
    SessionFailed {
        error: String,
    },
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
}

impl std::fmt::Display for AttemptSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SnapshotFailed { error } => write!(f, "Failed to snapshot brain state: {error}"),
            Self::WorktreeFailed { error } => write!(f, "Failed to create worktree: {error}"),
            Self::InitFailed { error } => write!(f, "Failed to initialize worker: {error}"),
            Self::SessionFailed { error } => write!(f, "Failed to create worker session: {error}"),
            Self::OverlayConflict {
                source_task_id,
                files,
            } => write!(
                f,
                "overlay conflict applying {source_task_id}: {} files",
                files.len()
            ),
        }
    }
}

/// Policy for what to apply when a review gate's timeout fires.
///
/// Shared by `AgentReviewPolicy::review_timeout_default` (config input)
/// and `DelegationStatus::TimedOut.fallback` (status discriminant).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeoutFallback {
    /// Auto-approve — worker's diff/summary retained as if reviewed.
    Approve,
    /// Auto-reject — carries the configured reason.
    Reject { reason: String },
    /// Explicit "nobody reviewed" signal (headless/batch modes).
    Abandon,
}

/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
    /// Structured diff stats (files changed, lines added/removed, file list).
    /// Populated from `git diff --numstat` at result-construction time.
    /// `None` when the worker produced no diff (setup failure, empty diff,
    /// or the diff call failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<DiffSummary>,
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_branch: Option<String>,
    /// Side-channel reference to persisted worker stdout when `summary`
    /// would otherwise lose bytes to `truncate_summary`. See
    /// `crate::domain::artifact::WorkerArtifact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<crate::domain::artifact::WorkerArtifact>,
}

/// Structured reasoning trace the brain passes alongside each
/// `delegate_to_worker` / `delegate_parallel` call. All fields optional;
/// permissive schema. See design spec section C.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct DelegationPlan {
    /// Candidate agents the brain considered.
    #[serde(default)]
    pub candidates: Vec<PlanCandidate>,
    /// Subtask breakdown for multi-task dispatches.
    #[serde(default)]
    pub decomposition: Vec<PlanSubtask>,
    /// The agent the brain committed to (or "self"/"parallel").
    pub chosen: Option<String>,
    /// Short justification surfaced to the review gate.
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PlanCandidate {
    pub agent: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PlanSubtask {
    pub subtask: Option<String>,
    #[serde(default)]
    pub parallelizable_with: Vec<String>,
}

/// Serializes `Duration` as whole seconds (`u64`).
///
/// Sub-second precision is intentionally discarded — `waited_for` is
/// derived from `review_timeout`, a config value in whole seconds. Do
/// not use this module for durations where sub-second precision matters.
mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

// ─── INV-6: Cancellation primitives ──────────────────────────────────────────

/// Outcome returned by `CancellationControl::cancel`.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Token found and cancellation signaled. The delegation's
    /// `tokio::select!` will resolve to `DelegationStatus::Cancelled`.
    Cancelled,
    /// No token with this `request_id` — delegation already completed or
    /// was never dispatched.
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationAbortReason {
    BrainRequested {
        reason: String,
    },
    WorkerHeartbeatTimeout {
        executor_id: String,
        idle_for_secs: u64,
    },
}

#[derive(Clone)]
pub struct DelegationAbortHandle {
    token: CancellationToken,
    reason: Arc<tokio::sync::Mutex<Option<DelegationAbortReason>>>,
}

impl DelegationAbortHandle {
    pub fn new(token: CancellationToken) -> Self {
        Self {
            token,
            reason: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub async fn request_abort(&self, reason: DelegationAbortReason) {
        let mut guard = self.reason.lock().await;
        if guard.is_none() {
            *guard = Some(reason);
            self.token.cancel();
        }
    }

    pub async fn observed_reason(&self) -> Option<DelegationAbortReason> {
        self.reason.lock().await.clone()
    }
}

/// Clonable handle to the per-delegation cancellation token registry.
///
/// Obtained from `Orchestrator::cancellation_control()`. Pass a clone to
/// `McpCallbackServer` so `handle_cancel_delegation` can reach it without
/// routing through the normal `DelegationRequest` channel.
#[derive(Clone, Default)]
pub struct CancellationControl {
    tokens: Arc<Mutex<HashMap<String, DelegationAbortHandle>>>,
}

impl CancellationControl {
    /// Create a new, empty control handle. The orchestrator creates one and
    /// hands out clones.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh token for `request_id`. Called by
    /// `handle_delegations` before spawning the delegation task.
    pub async fn register(&self, request_id: String) -> CancellationToken {
        let token = CancellationToken::new();
        let handle = DelegationAbortHandle::new(token.clone());
        self.tokens.lock().await.insert(request_id, handle);
        token
    }

    /// Register a fresh token plus its paired typed abort handle.
    pub async fn register_with_abort_handle(
        &self,
        request_id: String,
    ) -> (CancellationToken, DelegationAbortHandle) {
        let token = CancellationToken::new();
        let handle = DelegationAbortHandle::new(token.clone());
        self.tokens.lock().await.insert(request_id, handle.clone());
        (token, handle)
    }

    /// Remove and cancel the token for `request_id`.
    /// Returns `Cancelled` if the token was found, `NotFound` otherwise.
    pub async fn cancel(&self, request_id: &str) -> CancelOutcome {
        self.cancel_with_reason(request_id, "brain requested cancel".into())
            .await
    }

    /// Remove the token entry and cancel with a typed brain-requested reason.
    pub async fn cancel_with_reason(&self, request_id: &str, reason: String) -> CancelOutcome {
        if let Some(handle) = self.tokens.lock().await.remove(request_id) {
            handle
                .request_abort(DelegationAbortReason::BrainRequested { reason })
                .await;
            CancelOutcome::Cancelled
        } else {
            CancelOutcome::NotFound
        }
    }

    /// Remove the token entry without cancelling (called after normal
    /// completion so stale entries don't accumulate).
    pub async fn remove(&self, request_id: &str) {
        self.tokens.lock().await.remove(request_id);
    }
}

#[cfg(test)]
mod delegation_result_tests {
    use super::*;
    use crate::DiffSummary;
    use std::path::PathBuf;

    #[test]
    fn delegation_plan_deserializes_from_full_json() {
        let json = r#"{
            "candidates": [
                {"agent": "claude", "rationale": "default fit"},
                {"agent": "codex", "rationale": "cheaper alternative"}
            ],
            "decomposition": [
                {"subtask": "refactor auth", "parallelizable_with": ["refactor tests"]}
            ],
            "chosen": "claude",
            "rationale": "Scope > 3 files; claude is generalist."
        }"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.chosen.as_deref(), Some("claude"));
        assert_eq!(plan.candidates.len(), 2);
        assert_eq!(plan.decomposition.len(), 1);
        assert!(plan.rationale.is_some());
    }

    #[test]
    fn delegation_plan_deserializes_from_minimal_json() {
        let json = r#"{"chosen": "kiro", "rationale": "spec work"}"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.chosen.as_deref(), Some("kiro"));
        assert!(plan.candidates.is_empty());
        assert!(plan.decomposition.is_empty());
    }

    #[test]
    fn delegation_plan_deserializes_from_empty_json() {
        let json = r#"{}"#;
        let plan: DelegationPlan = serde_json::from_str(json).unwrap();
        assert!(plan.chosen.is_none());
        assert!(plan.candidates.is_empty());
    }

    #[test]
    fn result_with_diff_summary_round_trips_json() {
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: Some("--- a/x\n+++ b/x\n".into()),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 3,
                deletions: 1,
                files: vec![PathBuf::from("x")],
            }),
            summary: Some("did the thing".into()),
            estimated_cost_usd: 0.42,
            worker_branch: None,
            artifact: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: DelegationResult = serde_json::from_str(&json).unwrap();
        let ds = back.diff_summary.expect("diff_summary should round-trip");
        assert_eq!(ds.files_changed, 1);
        assert_eq!(ds.insertions, 3);
        assert_eq!(ds.files, vec![PathBuf::from("x")]);
    }

    #[test]
    fn result_without_diff_summary_deserializes_old_payloads() {
        // Older payloads omit the field entirely. serde must accept.
        let json = r#"{"status":"Success","diff":null,"summary":null,"estimated_cost_usd":0.0}"#;
        let back: DelegationResult = serde_json::from_str(json).unwrap();
        assert!(back.diff_summary.is_none());
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use crate::domain::artifact::{ArtifactKind, WorkerArtifact};

    #[test]
    fn delegation_result_omits_artifact_when_none() {
        let r = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("ok".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("\"artifact\""),
            "artifact should be omitted when None: {json}"
        );
    }

    #[test]
    fn delegation_result_round_trips_with_artifact() {
        let art = WorkerArtifact {
            object_ref: "refs/spur/artifacts/s1".into(),
            blob_sha: "a".repeat(40),
            size_bytes: 10_000,
            kind: ArtifactKind::Output,
        };
        let r = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("truncated".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: Some(art.clone()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: DelegationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.artifact, Some(art));
    }
}
