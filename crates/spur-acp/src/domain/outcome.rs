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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeBlobKind {
    #[default]
    ResultJson,
    RawStdout,
}

impl OutcomeBlobKind {
    pub fn as_ref_component(self) -> &'static str {
        match self {
            Self::ResultJson => "result-json",
            Self::RawStdout => "raw-stdout",
        }
    }
}

/// Identifier for a single delegation outcome blob.
///
/// Granularity is `(brain_session, delegation, attempt, kind)` — each retry
/// gets its own key so historical outcomes are addressable. Round 11
/// (MF1) — earlier per-session granularity caused overwrites.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub brain_session_id: BrainSessionId,
    pub delegation_id: DelegationId,
    pub attempt: u32,
    #[serde(default)]
    pub kind: OutcomeBlobKind,
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
    /// 40-char hex SHA-1 of the underlying git blob, when the backend is
    /// `BackendTag::GitBlob`. Phase 1's `fetch_outcome_artifact` MCP tool
    /// resolves blobs via `git cat-file -p <git_blob_sha>`, which expects
    /// the git object SHA-1 — distinct from the content SHA-256 stored in
    /// `OutcomeRef.sha256`. Non-git backends leave this as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_blob_sha: Option<String>,
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
    /// `byte_size` saturates to `usize::MAX` on 32-bit targets where an
    /// outcome larger than 4 GiB would otherwise wrap silently. Worker
    /// artifacts are bounded by the Plan-4 truncation ladder well below
    /// that; saturation is defensive — callers won't see it in practice.
    pub fn as_worker_artifact(&self, kind: WorkerArtifactKind) -> Option<WorkerArtifact> {
        match (&self.backend, &self.git_blob_sha) {
            (BackendTag::GitBlob, Some(git_sha)) => Some(WorkerArtifact {
                object_ref: format!(
                    "refs/spur/outcomes/{}/{}-{}-{}.blob",
                    self.key.brain_session_id,
                    self.key.delegation_id,
                    self.key.attempt,
                    self.key.kind.as_ref_component(),
                ),
                blob_sha: git_sha.clone(),
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
            kind: OutcomeBlobKind::ResultJson,
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
            git_blob_sha: None,
        };
        let s = serde_json::to_string(&r).expect("serialize");
        let back: OutcomeRef = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.sha256, r.sha256);
        assert_eq!(back.byte_size, r.byte_size);
        assert_eq!(back.backend, BackendTag::Fs);
        assert!(back.git_blob_sha.is_none());
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
    fn outcome_blob_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&OutcomeBlobKind::ResultJson).expect("ser"),
            "\"result_json\""
        );
        assert_eq!(
            serde_json::to_string(&OutcomeBlobKind::RawStdout).expect("ser"),
            "\"raw_stdout\""
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
            git_blob_sha: Some("c".repeat(40)),
        };
        let wa = r
            .as_worker_artifact(WorkerArtifactKind::Output)
            .expect("git_blob backend should map");
        assert_eq!(
            wa.object_ref,
            "refs/spur/outcomes/550e8400-e29b-41d4-a716-446655440000/\
             deadbeef-1111-2222-3333-444455556666-1-result-json.blob"
        );
        // blob_sha must be the 40-char git SHA-1, NOT the content SHA-256.
        // Phase 1's fetch_outcome_artifact runs `git cat-file -p blob_sha`.
        assert_eq!(wa.blob_sha, "c".repeat(40));
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
            git_blob_sha: Some("c".repeat(40)),
        };
        let wa = r
            .as_worker_artifact(WorkerArtifactKind::Diagnostic)
            .expect("git_blob backend should map");
        assert_eq!(
            wa.object_ref,
            "refs/spur/outcomes/550e8400-e29b-41d4-a716-446655440000/\
             deadbeef-1111-2222-3333-444455556666-5-result-json.blob"
        );
        assert_eq!(wa.kind, WorkerArtifactKind::Diagnostic);
    }

    #[test]
    fn as_worker_artifact_returns_none_when_git_blob_sha_missing() {
        // GitBlob backend without git_blob_sha is an upstream invariant
        // violation; adapter returns None defensively rather than producing
        // a WorkerArtifact whose blob_sha is unfetchable by Phase 1's tool.
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 99,
            backend: BackendTag::GitBlob,
            git_blob_sha: None,
        };
        assert!(r.as_worker_artifact(WorkerArtifactKind::Output).is_none());
    }

    #[test]
    fn as_worker_artifact_returns_none_for_fs_backend() {
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 99,
            backend: BackendTag::Fs,
            git_blob_sha: None,
        };
        assert!(r.as_worker_artifact(WorkerArtifactKind::Output).is_none());
    }
}
