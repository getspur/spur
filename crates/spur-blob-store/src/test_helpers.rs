//! Test-only helpers gated behind the `test-support` feature.
//!
//! Consumer crates depend on this module via:
//!
//! ```toml
//! [dev-dependencies]
//! spur-blob-store = { workspace = true, features = ["test-support"] }
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use spur_acp::BrainSessionId;

use crate::trait_def::OutcomeStore;
use crate::{
    DeleteNamespaceReport, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section,
    StoreError, SweepReport,
};

/// Failure mode the mock injects on every operation. Each enumerant maps
/// to a distinct `StoreError` so tests can assert materializer behavior
/// per failure surface, not just one example.
///
/// **No `Panic` variant**: while spec §7.7 (Round 9 P3-S3) lists "panic
/// inside put — exercises materializer's panic catching" as desirable,
/// `OutcomeStore::put` is `async` and `tokio::task::spawn` + `JoinHandle`
/// `catch_unwind` plumbing belongs in the materializer (production
/// concern), not the test mock. Panic resilience is covered by a
/// dedicated test in `crates/spur-mcp/src/outcome_materializer.rs` that
/// constructs an inline async closure that panics — the mock stays
/// `Result`-pure.
#[derive(Debug, Clone)]
pub enum FailureMode {
    Io,
    TooLarge,
    Backend(String),
    ContentMismatch,
}

/// `OutcomeStore` impl that always fails per `FailureMode`. Used to exercise
/// the materializer's truncation-ladder fallback path. Panic resilience is
/// covered by a dedicated test in the materializer (T6), not here.
#[derive(Debug, Clone)]
pub struct MockFailingOutcomeStore {
    pub mode: FailureMode,
}

impl MockFailingOutcomeStore {
    /// Returns `Arc<Self>` (not `Arc<dyn OutcomeStore>`) so callers retain
    /// access to `pub mode`. Unsized coercion to `Arc<dyn OutcomeStore>`
    /// happens automatically at the call site (e.g., when wiring the
    /// materializer in T6).
    pub fn new(mode: FailureMode) -> Arc<Self> {
        Arc::new(Self { mode })
    }

    fn err(&self, key: &OutcomeKey) -> StoreError {
        match &self.mode {
            FailureMode::Io => StoreError::Io(std::io::Error::other("mock io")),
            FailureMode::TooLarge => StoreError::TooLarge {
                actual: 1_000_000,
                limit: 1024,
            },
            FailureMode::Backend(s) => StoreError::Backend(s.clone()),
            FailureMode::ContentMismatch => StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: "a".repeat(64),
                new_sha: "b".repeat(64),
            },
        }
    }
}

#[async_trait]
impl OutcomeStore for MockFailingOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        _content: &[u8],
        _metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Err(self.err(key))
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        Err(self.err(key))
    }

    async fn delete_namespace(
        &self,
        _brain_session_id: &BrainSessionId,
    ) -> Result<DeleteNamespaceReport, StoreError> {
        match &self.mode {
            FailureMode::Io => Err(StoreError::Io(std::io::Error::other("mock io"))),
            _ => Err(StoreError::Backend("mock delete_namespace failure".into())),
        }
    }

    async fn sweep_older_than(&self, _ttl: Duration) -> Result<SweepReport, StoreError> {
        Err(StoreError::Backend("mock sweep failure".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OutcomeBlobKind;
    use spur_acp::SessionId;

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

    #[tokio::test]
    async fn mock_returns_io_error() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::Io,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::Io(_)));
    }

    #[tokio::test]
    async fn mock_returns_content_mismatch() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::ContentMismatch,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::ContentMismatch { .. }));
    }

    #[tokio::test]
    async fn mock_returns_too_large() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::TooLarge,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn mock_returns_backend_error_with_message() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::Backend("git update-ref failed".into()),
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        match err {
            StoreError::Backend(msg) => assert_eq!(msg, "git update-ref failed"),
            e => panic!("expected Backend, got {e:?}"),
        }
    }
}
