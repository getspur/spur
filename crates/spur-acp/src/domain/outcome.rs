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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(serde_json::to_string(&BackendTag::Fs).expect("ser"), "\"fs\"");
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
}
