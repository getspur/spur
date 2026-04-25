//! Single producer of `BrainContinuation` for completed delegations.
//!
//! See `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md`
//! §7.2 for the full design. The materializer runs persist-then-clip-then-build:
//!
//! 1. Persist the full `DelegationResult` to `OutcomeStore`.
//! 2. On store-put success: clip a copy of the inline fields and build a lean
//!    `BrainContinuation` with `artifact_id: Some(...)`.
//! 3. On store-put failure: fall through to the Plan-4 truncation-ladder
//!    fallback (see `spur_core::continuation_bridge`).
//!
//! INV-D8 (envelope ≤ MERGE_BUDGET) is enforced by clip + a release-mode
//! `if envelope_bytes > budget` recovery branch into the truncation ladder.
//! `debug_assert!` catches violations loudly in tests.

use std::sync::Arc;

use spur_acp::domain::{
    ArtifactRef, BrainContinuation, ContinuationPayload, ContinuationSource, DelegationId,
    DelegationResult, OutcomeKey,
};
use spur_acp::BrainSessionId;
use spur_blob_store::{OutcomeStore, StoreError};

use crate::events::McpEventSink;

/// Default cap counts. These match Plan-4's truncation ladder.
pub const DEFAULT_SUMMARY_CAP_BYTES: usize = 512;
pub const DEFAULT_WORKER_BRANCH_CAP_BYTES: usize = 256;
pub const DEFAULT_FETCH_HINT_CAP_BYTES: usize = 256;
pub const DEFAULT_DIFF_FILES_CAP_COUNT: usize = 16;
pub const DEFAULT_STATUS_STRING_CAP_BYTES: usize = 512;
pub const DEFAULT_ARTIFACT_REF_STRING_CAP_BYTES: usize = 256;

#[derive(Clone)]
pub struct OutcomeMaterializer {
    store: Arc<dyn OutcomeStore>,
    /// Tracks the highest attempt seen per delegation so the fetch tool
    /// can default `attempt` to "latest known". Lives on the materializer
    /// (not the server) so both the direct callback path
    /// (server.rs::build_detached_continuation) and the reconciler path
    /// (plan/mod.rs::persist_completion_result_and_notify) — the latter
    /// only has `&OutcomeMaterializer` access — can update the map.
    /// Memory bound: ~40 B per delegation. A long-running brain session
    /// with thousands of completions sits in tens of KB; not pruned.
    latest_attempt_by_delegation: Arc<tokio::sync::Mutex<
        std::collections::HashMap<DelegationId, u32>
    >>,
    summary_cap_bytes: usize,
    worker_branch_cap_bytes: usize,
    fetch_hint_cap_bytes: usize,
    diff_files_cap_count: usize,
    status_string_cap_bytes: usize,
    artifact_ref_string_cap_bytes: usize,
}

impl std::fmt::Debug for OutcomeMaterializer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutcomeMaterializer")
            .field("summary_cap_bytes", &self.summary_cap_bytes)
            .field("worker_branch_cap_bytes", &self.worker_branch_cap_bytes)
            .field("status_string_cap_bytes", &self.status_string_cap_bytes)
            .finish_non_exhaustive()
    }
}

impl OutcomeMaterializer {
    pub fn new(store: Arc<dyn OutcomeStore>) -> Self {
        Self {
            store,
            latest_attempt_by_delegation: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            summary_cap_bytes: DEFAULT_SUMMARY_CAP_BYTES,
            worker_branch_cap_bytes: DEFAULT_WORKER_BRANCH_CAP_BYTES,
            fetch_hint_cap_bytes: DEFAULT_FETCH_HINT_CAP_BYTES,
            diff_files_cap_count: DEFAULT_DIFF_FILES_CAP_COUNT,
            status_string_cap_bytes: DEFAULT_STATUS_STRING_CAP_BYTES,
            artifact_ref_string_cap_bytes: DEFAULT_ARTIFACT_REF_STRING_CAP_BYTES,
        }
    }

    /// Look up the highest attempt the materializer has materialized for
    /// `delegation_id`. Used by `fetch_outcome_artifact` (Task 10) when
    /// the caller doesn't pin a specific attempt.
    pub async fn latest_attempt(&self, delegation_id: &DelegationId) -> Option<u32> {
        let map = self.latest_attempt_by_delegation.lock().await;
        map.get(delegation_id).copied()
    }

    /// Builder methods for tests that need to exercise truncation paths
    /// without allocating multi-KB strings. Production callers should use
    /// `new()` + accept the defaults.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_status_string_cap(mut self, cap: usize) -> Self {
        self.status_string_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_summary_cap(mut self, cap: usize) -> Self {
        self.summary_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_diff_files_cap(mut self, cap: usize) -> Self {
        self.diff_files_cap_count = cap;
        self
    }

    /// Single entrypoint for both completion call sites (§7.3). Persists the
    /// full result to OutcomeStore, then builds a lean `BrainContinuation`.
    /// On persist failure, falls through to the Plan-4 truncation ladder.
    pub async fn materialize(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        // Method body lands in Task 5 (success path) + Task 6 (fallback).
        // Skeleton returns a placeholder so downstream wiring tasks compile.
        let _ = (result, delegation_id, attempt, brain_session, source, event_sink);
        unimplemented!("Task 5 wires the persist-then-clip-then-build success path");
    }
}

/// Hint string surfaced to the brain when `artifact_id` is `Some(_)`.
/// Built from clipped status + diff so the brain knows which `section` to
/// fetch first. Capped at `fetch_hint_cap_bytes`.
#[allow(dead_code)]
pub(crate) fn build_fetch_hint(_status_clipped: bool, _diff_files_clipped: bool) -> String {
    // Body lands in Task 5.
    unimplemented!("Task 5 implements build_fetch_hint")
}
