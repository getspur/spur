//! Side-channel artifact persisted for a worker attempt.
//!
//! When a worker's stdout exceeds `SPUR_SUMMARY_MAX_BYTES`, the
//! orchestrator persists the full output as a git blob under
//! `refs/spur/artifacts/<session-id>` and surfaces a `WorkerArtifact`
//! on `DelegationResult` so the brain can retrieve the full text via
//! `git cat-file -p <object_ref>` (or `<blob_sha>`) without violating
//! the per-delegation context budget.
//!
//! This is a *retrievable-only* channel. Artifacts are never merged
//! into user repo state and never appear on `worker_branch`.

use serde::{Deserialize, Serialize};

/// What kind of output this artifact captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Worker stdout for a successful delegation.
    Output,
    /// Worker stdout for a failing delegation — preserves full error
    /// context that would otherwise be truncated.
    Diagnostic,
}

/// Side-channel reference to a worker's persisted stdout.
///
/// `object_ref` is a human-readable git ref path; `blob_sha` is the
/// stable SHA-1 of the blob (survives ref deletion until git GC).
/// Retrieve via `git cat-file -p <object_ref>` or `git cat-file -p <blob_sha>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerArtifact {
    /// e.g. `"refs/spur/artifacts/<session-id>"`
    pub object_ref: String,
    /// 40-char hex SHA-1 of the blob.
    pub blob_sha: String,
    /// Size in bytes of the PERSISTED content (post stored-cap truncation).
    pub size_bytes: usize,
    pub kind: ArtifactKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ArtifactKind::Output).unwrap(),
            "\"output\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactKind::Diagnostic).unwrap(),
            "\"diagnostic\""
        );
    }

    #[test]
    fn artifact_round_trips_through_serde() {
        let a = WorkerArtifact {
            object_ref: "refs/spur/artifacts/abc123".into(),
            blob_sha: "0".repeat(40),
            size_bytes: 18_432,
            kind: ArtifactKind::Output,
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: WorkerArtifact = serde_json::from_str(&s).unwrap();
        assert_eq!(back.object_ref, a.object_ref);
        assert_eq!(back.blob_sha, a.blob_sha);
        assert_eq!(back.size_bytes, a.size_bytes);
        assert!(matches!(back.kind, ArtifactKind::Output));
    }
}
