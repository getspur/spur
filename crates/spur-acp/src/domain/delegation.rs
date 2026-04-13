use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Result status of a delegation to a worker.
///
/// `Rejected` is reserved for human-issued rejections arriving via the
/// review gate. System-applied timeouts use `TimedOut` so the brain can
/// distinguish actionable feedback from "nobody reviewed in time."
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DelegationStatus {
    // Pre-existing worker-level variants.
    Success,
    Failed { error: String },
    Conflict { files: Vec<PathBuf> },
    /// Worker hung past the hard worker-hang deadline (distinct from
    /// review-gate timeout, which is `TimedOut`).
    Timeout,

    // Review-gate variants.
    /// Human reviewer rejected the work. `reason` is actionable feedback
    /// the brain can address on a retry.
    Rejected { reason: String },
    /// Human reviewer approved-with-modifications; `reviewer_note` is a
    /// caveat the brain should consider alongside the accepted diff.
    Modified { reviewer_note: String },
    /// Review timeout fired. `fallback` records the configured
    /// `TimeoutFallback` that was applied.
    TimedOut {
        #[serde(with = "duration_serde")]
        waited_for: Duration,
        fallback: TimeoutFallback,
    },
}

/// Policy for what to apply when a review gate's timeout fires.
///
/// Shared by `AgentReviewPolicy::review_timeout_default` (config input)
/// and `DelegationStatus::TimedOut.fallback` (status discriminant).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
}

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
