//! The `OutcomeStore` trait — content-addressed blob storage for
//! delegation outcomes.
//!
//! Implementations:
//! - `MemoryOutcomeStore` (test/dev) — in-process `HashMap`.
//! - `FsOutcomeStore` (default for new outcomes in production).
//! - `GitBlobOutcomeStore` (lives in `spur-worktree` to keep git
//!   knowledge in one place; depends on this crate for the trait).
//! - `MeasuredOutcomeStore<S>` decorator that emits `tracing` events.

use std::time::Duration;

use async_trait::async_trait;
use spur_acp::BrainSessionId;

use crate::{OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError, SweepReport};

/// Content-addressed outcome blob storage.
///
/// All methods are idempotent where stated and **must** be safe to call
/// concurrently from multiple async tasks (the trait requires `Send + Sync`).
#[async_trait]
pub trait OutcomeStore: Send + Sync {
    /// Store `content` under `key`. Idempotent: two `put` calls with
    /// the same key + the same content return the same `OutcomeRef`
    /// without rewriting. Differing content under the same key is an
    /// upstream invariant violation: returns
    /// [`StoreError::ContentMismatch`] (Round 11 SF1).
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError>;

    /// Read the content at `key`. `section` is currently `Some(Section::Full)`
    /// or `None` (treated as `Full`); Phase 3 widens the section selector.
    /// Implementations MAY reject non-`Full` sections with
    /// `StoreError::Backend("section not supported")` until Phase 3 lands.
    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError>;

    /// Delete every blob owned by `brain_session_id`. Returns the
    /// number of blobs deleted (zero is allowed). Used on session
    /// teardown.
    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError>;

    /// Sweep namespaces whose newest artifact is older than `ttl`.
    /// `FsOutcomeStore` requires `ttl >= 1 day` (Round 9 P2-S3); other
    /// impls may relax this.
    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError>;
}
