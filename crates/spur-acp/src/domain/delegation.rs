use crate::DiffSummary;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

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

#[cfg(test)]
mod delegation_result_tests {
    use super::*;
    use crate::DiffSummary;
    use std::path::PathBuf;

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
