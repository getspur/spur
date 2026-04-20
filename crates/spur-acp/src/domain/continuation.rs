use std::time::Instant;

use crate::domain::delegation::{DelegationId, DelegationStatus};
use crate::domain::events::DiffSummary;

/// Why SPUR is re-entering the brain with a continuation turn.
///
/// See `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuationSource {
    /// Originating call was `delegate_async`.
    AsyncRequested,
    /// `delegate_to_worker` exceeded the MCP block window; returned
    /// `delegation_id` for polling; worker later finished.
    BlockTimeout,
    /// Worker reached `DelegationStatus::Cancelled` (INV-6).
    Cancelled,
    /// `SpurEventBody::PlanCompleted` fired for a plan the brain dispatched.
    PlanCompleted,
    /// `SpurEventBody::PlanReadyToMerge` fired for a plan the brain dispatched.
    PlanReadyToMerge,
}

/// Narrow projection of a worker outcome for scheduler consumption.
///
/// Deliberately NOT `DelegationResult` to decouple scheduler evolution
/// from result-struct evolution and to avoid moving large diffs through
/// the orchestrator ingress channel.
#[derive(Debug, Clone)]
pub struct ContinuationPayload {
    pub status: DelegationStatus,
    pub summary: Option<String>,
    pub diff_summary: Option<DiffSummary>,
    pub worker_branch: Option<String>,
    /// Side-channel reference to persisted stdout; see
    /// `crate::domain::artifact::WorkerArtifact`.
    pub artifact: Option<crate::domain::artifact::WorkerArtifact>,
}

/// One detached delegation result awaiting brain re-entry.
#[derive(Debug, Clone)]
pub struct BrainContinuation {
    /// Correlation key.
    pub delegation_id: DelegationId,
    /// Why this continuation fired.
    pub source: ContinuationSource,
    /// Narrow projection of the worker outcome.
    pub payload: ContinuationPayload,
    /// Monotonic creation time; not persisted across process restart.
    /// Equality on `BrainContinuation` should be done via `delegation_id`, not this field.
    pub created_at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::delegation::DelegationStatus;
    use std::time::Instant;

    #[test]
    fn continuation_payload_builds_from_parts() {
        let p = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("done".into()),
            diff_summary: None,
            worker_branch: Some("wt/abc".into()),
            artifact: None,
        };
        assert_eq!(p.summary.as_deref(), Some("done"));
        assert!(matches!(p.status, DelegationStatus::Success));
    }

    #[test]
    fn continuation_source_variants_exhaustive() {
        // Manual count guard: update when adding ContinuationSource variants (#[non_exhaustive] means the compiler won't flag omissions here).
        let vs = [
            ContinuationSource::AsyncRequested,
            ContinuationSource::BlockTimeout,
            ContinuationSource::Cancelled,
            ContinuationSource::PlanCompleted,
            ContinuationSource::PlanReadyToMerge,
        ];
        assert_eq!(vs.len(), 5);
    }

    #[test]
    fn brain_continuation_holds_delegation_id_and_source() {
        let c = BrainContinuation {
            delegation_id: "uuid-1".into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
                artifact: None,
            },
            created_at: Instant::now(),
        };
        assert_eq!(c.delegation_id.as_str(), "uuid-1");
        assert!(matches!(c.source, ContinuationSource::AsyncRequested));
    }

    #[test]
    fn continuation_payload_carries_artifact() {
        use crate::domain::artifact::{ArtifactKind, WorkerArtifact};
        let art = WorkerArtifact {
            object_ref: "refs/spur/artifacts/s2".into(),
            blob_sha: "b".repeat(40),
            size_bytes: 20_000,
            kind: ArtifactKind::Diagnostic,
        };
        let p = ContinuationPayload {
            status: DelegationStatus::Failed {
                error: "boom".into(),
            },
            summary: Some("truncated error tail".into()),
            diff_summary: None,
            worker_branch: None,
            artifact: Some(art.clone()),
        };
        assert_eq!(p.artifact.as_ref().unwrap().size_bytes, 20_000);
        assert!(matches!(
            p.artifact.as_ref().unwrap().kind,
            ArtifactKind::Diagnostic
        ));
    }

    #[test]
    fn continuation_payload_preserves_artifact_through_clone() {
        use crate::domain::artifact::{ArtifactKind, WorkerArtifact};
        let art = WorkerArtifact {
            object_ref: "refs/spur/artifacts/s3".into(),
            blob_sha: "c".repeat(40),
            size_bytes: 12_345,
            kind: ArtifactKind::Output,
        };
        let p = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("pointer".into()),
            diff_summary: None,
            worker_branch: None,
            artifact: Some(art.clone()),
        };
        let copied = p.clone();
        assert_eq!(copied.artifact.as_ref().unwrap().blob_sha, art.blob_sha);
    }
}
