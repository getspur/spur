//! Wire-shape types for SPUR's content-addressed outcome storage.
//!
//! Lives in `spur-acp` (not `spur-blob-store`) so that
//! `ContinuationPayload.artifact_id: Option<OutcomeKey>` can reference
//! these types without forcing `spur-acp` to depend on `spur-blob-store`.
//! The trait, store-only types, and impls live in `spur-blob-store` (and
//! `spur-worktree::git_blob_store` for the git backend).
//!
//! Spec: `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §6.3.

use serde::{Deserialize, Serialize};

use crate::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};
use crate::{BrainSessionId, DelegationId};

/// Identifier for a single delegation outcome blob.
///
/// Granularity is `(brain_session, delegation, attempt)` — each retry
/// gets its own key so historical outcomes are addressable. Round 11
/// (MF1) — earlier per-session granularity caused overwrites.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub brain_session_id: BrainSessionId,
    pub delegation_id: DelegationId,
    pub attempt: u32,
}

/// Identifies which storage backend produced this outcome blob.
///
/// **NOT `Copy`** — Round 9 (P2-S1). Future cloud variants will need
/// to carry `String` config (region, bucket); removing `Copy` later
/// would be a breaking change. Doing it now costs one `.clone()` per use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendTag {
    Fs,
    GitBlob,
    // Future: Cloud { region: String, bucket: String }, ...
}

/// Strong reference to a stored outcome blob, returned by
/// `OutcomeStore::put`.
///
/// Carries the SHA-256 hash of the stored content (single source of
/// truth — Round 11 SF1) and the backend tag so consumers can branch
/// on backend-specific affordances (e.g., git-blob retrieval).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRef {
    pub key: OutcomeKey,
    /// 64-char lowercase hex of the stored content's SHA-256 digest.
    pub sha256: String,
    /// Size in bytes of the STORED content (post-truncation if applicable).
    pub byte_size: u64,
    pub backend: BackendTag,
}

impl OutcomeRef {
    /// Backcompat adapter: project a GitBlob-backed `OutcomeRef` into
    /// the legacy `WorkerArtifact` shape. Returns `None` for non-git
    /// backends — the destination upstream (`DelegationResult.artifact`)
    /// is itself `Option<WorkerArtifact>`, so `None` cleanly signals "no
    /// git-blob projection" without ambiguity vs. a hard failure.
    ///
    /// `object_ref` is the per-(session, delegation, attempt) ref under
    /// `refs/spur/outcomes/`. The legacy `refs/spur/artifacts/<session>`
    /// ref is read-only during transition; new writes go to the new
    /// namespace.
    ///
    /// `byte_size` saturates to `usize::MAX` on 32-bit targets where a
    /// >4 GiB outcome would otherwise wrap silently. Worker artifacts
    /// are bounded by the Plan-4 truncation ladder well below that, so
    /// saturation is defensive — callers will never see it in practice.
    pub fn as_worker_artifact(&self, kind: WorkerArtifactKind) -> Option<WorkerArtifact> {
        match &self.backend {
            BackendTag::GitBlob => Some(WorkerArtifact {
                object_ref: format!(
                    "refs/spur/outcomes/{}/{}-{}.blob",
                    self.key.brain_session_id, self.key.delegation_id, self.key.attempt,
                ),
                blob_sha: self.sha256.clone(),
                size_bytes: usize::try_from(self.byte_size).unwrap_or(usize::MAX),
                kind,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::artifact::ArtifactKind as WorkerArtifactKind;
    use crate::SessionId;

    fn key() -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        }
    }

    #[test]
    fn outcome_key_round_trips_through_serde() {
        let k = key();
        let s = serde_json::to_string(&k).expect("serialize");
        let back: OutcomeKey = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, k);
    }

    #[test]
    fn outcome_ref_round_trips_through_serde() {
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 1024,
            backend: BackendTag::Fs,
        };
        let s = serde_json::to_string(&r).expect("serialize");
        let back: OutcomeRef = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.sha256, r.sha256);
        assert_eq!(back.byte_size, r.byte_size);
        assert_eq!(back.backend, BackendTag::Fs);
    }

    #[test]
    fn backend_tag_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&BackendTag::Fs).expect("ser"),
            "\"fs\""
        );
        assert_eq!(
            serde_json::to_string(&BackendTag::GitBlob).expect("ser"),
            "\"git_blob\""
        );
    }

    #[test]
    fn outcome_key_is_hashable() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(key());
        assert!(s.contains(&key()));
    }

    #[test]
    fn as_worker_artifact_maps_git_blob_backend_only() {
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 99,
            backend: BackendTag::GitBlob,
        };
        let wa = r
            .as_worker_artifact(WorkerArtifactKind::Output)
            .expect("git_blob backend should map");
        assert_eq!(
            wa.object_ref,
            "refs/spur/outcomes/550e8400-e29b-41d4-a716-446655440000/\
             deadbeef-1111-2222-3333-444455556666-1.blob"
        );
        assert_eq!(wa.blob_sha, r.sha256);
        assert_eq!(wa.size_bytes, 99);
        assert_eq!(wa.kind, WorkerArtifactKind::Output);
    }

    #[test]
    fn as_worker_artifact_uses_attempt_in_ref_path() {
        let mut k = key();
        k.attempt = 5;
        let r = OutcomeRef {
            key: k,
            sha256: "a".repeat(64),
            byte_size: 1,
            backend: BackendTag::GitBlob,
        };
        let wa = r
            .as_worker_artifact(WorkerArtifactKind::Diagnostic)
            .expect("git_blob backend should map");
        assert_eq!(
            wa.object_ref,
            "refs/spur/outcomes/550e8400-e29b-41d4-a716-446655440000/\
             deadbeef-1111-2222-3333-444455556666-5.blob"
        );
        assert_eq!(wa.kind, WorkerArtifactKind::Diagnostic);
    }

    #[test]
    fn as_worker_artifact_returns_none_for_fs_backend() {
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 99,
            backend: BackendTag::Fs,
        };
        assert!(r.as_worker_artifact(WorkerArtifactKind::Output).is_none());
    }
}
