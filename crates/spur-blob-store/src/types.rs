//! Store-internal types: metadata, sections, errors, sweep reports.
//!
//! These types are owned by `spur-blob-store` because they don't cross
//! the spur-acp boundary. Wire-shape types live in `spur-acp::domain::outcome`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use spur_acp::BrainSessionId;
use thiserror::Error;

use crate::OutcomeKey;

/// What kind of payload this artifact captures. Carried in
/// `OutcomeMetadata` so consumers can branch (e.g., the materializer
/// renders diffs differently from raw stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Diff,
    Stdout,
    Stderr,
    Json,
}

/// Sidecar metadata persisted alongside each blob. Single source of
/// truth for the SHA-256 (Round 11 SF1) — `ContentMismatch` detection
/// reads this directly rather than re-hashing the stored content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeMetadata {
    pub created_at: DateTime<Utc>,
    pub content_type: ContentType,
    pub original_byte_size: u64,
    /// Size of the STORED content (after stored-cap truncation).
    pub stored_byte_size: u64,
    /// Round 11 (SF1): SHA-256 hex of stored content. Single source of truth
    /// for `ContentMismatch` detection — read here, never re-hashed from disk.
    pub sha256: String,
}

/// Section selector for partial reads. Phase 2 supports the union
/// `Full`; Phase 3 adds the narrower variants used by the lean schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    StatusOnly,
    Summary,
    DiffOnly,
    Full,
}

/// What `OutcomeStore::get` returns. Tied to `OutcomeMetadata.content_type`.
#[derive(Debug, Clone)]
pub struct OutcomeContent {
    pub bytes: Vec<u8>,
    pub metadata: OutcomeMetadata,
}

/// Reported by `OutcomeStore::sweep_older_than`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Number of namespaces (i.e., distinct `brain_session_id`s) deleted.
    pub namespaces_swept: usize,
    /// Number of individual blob+meta pairs deleted.
    pub blobs_swept: usize,
    /// Total bytes freed (sum of `stored_byte_size`).
    pub bytes_freed: u64,
    /// Effective TTL the store enforced. Never less than `Duration::from_secs(86_400)`
    /// for `FsOutcomeStore` (Round 9 P2-S3 — sub-day TTLs unsupported).
    pub effective_ttl: Duration,
}

/// Result of `OutcomeStore::delete_namespace`. Phase 4 added `total_bytes`
/// per spec §10.1 so the `outcome_namespace_deleted` metric can report
/// reclaimed disk usage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteNamespaceReport {
    pub count: usize,
    pub total_bytes: u64,
}

/// All errors `OutcomeStore` impls can return.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0:?}")]
    NotFound(OutcomeKey),
    #[error("authorization: caller session != artifact session (requested={requested:?}, actual={actual:?})")]
    Unauthorized {
        requested: OutcomeKey,
        actual: BrainSessionId,
    },
    #[error("content too large: {actual} > {limit}")]
    TooLarge { actual: u64, limit: u64 },
    /// Round 9 (N2) + Round 11 (SF1): same key, different content.
    /// Surfaces an upstream invariant violation: each
    /// `(brain_session, delegation, attempt)` triple should produce
    /// exactly one content blob.
    #[error("content mismatch for {key:?}: existing sha={existing_sha}, new sha={new_sha}")]
    ContentMismatch {
        key: OutcomeKey,
        existing_sha: String,
        new_sha: String,
    },
    /// Catch-all for backend-specific failures (e.g., `git update-ref`
    /// failed, S3 returned 5xx). The string is human-readable for logs.
    #[error("backend: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_metadata_round_trips_through_serde() {
        let m = OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: 2048,
            stored_byte_size: 1024,
            sha256: "a".repeat(64),
        };
        let s = serde_json::to_string(&m).expect("serialize");
        let back: OutcomeMetadata = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.content_type, ContentType::Stdout);
        assert_eq!(back.stored_byte_size, 1024);
        assert_eq!(back.sha256, m.sha256);
    }

    #[test]
    fn section_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Section::StatusOnly).expect("ser"),
            "\"status_only\""
        );
        assert_eq!(
            serde_json::to_string(&Section::DiffOnly).expect("ser"),
            "\"diff_only\""
        );
        assert_eq!(
            serde_json::to_string(&Section::Full).expect("ser"),
            "\"full\""
        );
    }

    #[test]
    fn store_error_renders_content_mismatch_clearly() {
        let key = OutcomeKey {
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
            kind: crate::OutcomeBlobKind::ResultJson,
        };
        let err = StoreError::ContentMismatch {
            key: key.clone(),
            existing_sha: "a".repeat(64),
            new_sha: "b".repeat(64),
        };
        let msg = format!("{err}");
        assert!(msg.contains("content mismatch"));
        assert!(msg.contains(&"a".repeat(64)));
        assert!(msg.contains(&"b".repeat(64)));
    }
}
