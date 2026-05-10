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
use spur_blob_store::OutcomeStore;

use crate::events::McpEventSink;

/// Default cap counts. These match Plan-4's truncation ladder.
pub const DEFAULT_SUMMARY_CAP_BYTES: usize = 512;
pub const DEFAULT_WORKER_BRANCH_CAP_BYTES: usize = 256;
pub const DEFAULT_FETCH_HINT_CAP_BYTES: usize = 256;
pub const DEFAULT_BASE_HINT_CAP_BYTES: usize = 256;
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
    /// (plan/mod.rs::persist_completion_inner) — the latter
    /// only has `&OutcomeMaterializer` access — can update the map.
    /// Memory bound: ~40 B per delegation. A long-running brain session
    /// with thousands of completions sits in tens of KB; not pruned.
    ///
    /// `std::sync::Mutex` (not `tokio::sync::Mutex`): the critical section
    /// is a HashMap get/insert with no `.await` inside, so the async
    /// scheduler-yielding mutex would only add overhead.
    latest_attempt_by_delegation:
        Arc<std::sync::Mutex<std::collections::HashMap<DelegationId, u32>>>,
    summary_cap_bytes: usize,
    worker_branch_cap_bytes: usize,
    fetch_hint_cap_bytes: usize,
    base_hint_cap_bytes: usize,
    diff_files_cap_count: usize,
    status_string_cap_bytes: usize,
    artifact_ref_string_cap_bytes: usize,
}

impl std::fmt::Debug for OutcomeMaterializer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutcomeMaterializer")
            .field("summary_cap_bytes", &self.summary_cap_bytes)
            .field("worker_branch_cap_bytes", &self.worker_branch_cap_bytes)
            .field("fetch_hint_cap_bytes", &self.fetch_hint_cap_bytes)
            .field("base_hint_cap_bytes", &self.base_hint_cap_bytes)
            .field("diff_files_cap_count", &self.diff_files_cap_count)
            .field("status_string_cap_bytes", &self.status_string_cap_bytes)
            .field(
                "artifact_ref_string_cap_bytes",
                &self.artifact_ref_string_cap_bytes,
            )
            .finish_non_exhaustive()
    }
}

impl OutcomeMaterializer {
    pub fn new(store: Arc<dyn OutcomeStore>) -> Self {
        Self {
            store,
            latest_attempt_by_delegation: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            summary_cap_bytes: DEFAULT_SUMMARY_CAP_BYTES,
            worker_branch_cap_bytes: DEFAULT_WORKER_BRANCH_CAP_BYTES,
            fetch_hint_cap_bytes: DEFAULT_FETCH_HINT_CAP_BYTES,
            base_hint_cap_bytes: DEFAULT_BASE_HINT_CAP_BYTES,
            diff_files_cap_count: DEFAULT_DIFF_FILES_CAP_COUNT,
            status_string_cap_bytes: DEFAULT_STATUS_STRING_CAP_BYTES,
            artifact_ref_string_cap_bytes: DEFAULT_ARTIFACT_REF_STRING_CAP_BYTES,
        }
    }

    /// Look up the highest attempt the materializer has materialized for
    /// `delegation_id`. Used by `fetch_outcome_artifact` (Task 10) when
    /// the caller doesn't pin a specific attempt.
    ///
    /// `async fn` is preserved for forward compatibility (T10 may move
    /// the lookup behind the OutcomeStore), even though the body is sync.
    pub async fn latest_attempt(&self, delegation_id: &DelegationId) -> Option<u32> {
        let map = self
            .latest_attempt_by_delegation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.get(delegation_id).copied()
    }

    /// Builder methods for tests that need to exercise truncation paths
    /// without allocating multi-KB strings. Production callers should use
    /// `new()` + accept the defaults.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use = "consuming builder; the returned value carries the override"]
    pub fn with_status_string_cap(mut self, cap: usize) -> Self {
        self.status_string_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    #[must_use = "consuming builder; the returned value carries the override"]
    pub fn with_summary_cap(mut self, cap: usize) -> Self {
        self.summary_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    #[must_use = "consuming builder; the returned value carries the override"]
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
        use futures::FutureExt;
        use spur_acp::domain::clip::{
            clip_artifact_ref_strings, clip_diff_files, clip_status_strings, clip_with_ellipsis,
        };
        use spur_blob_store::{ContentType, OutcomeMetadata};
        use std::panic::AssertUnwindSafe;
        use std::time::Instant;

        let start = Instant::now();

        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id: delegation_id.clone(),
            attempt,
        };

        let bytes = match serde_json::to_vec(&result) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = %error,
                    "result serialization failed"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
        };

        let sha = sha256_hex(&bytes);
        let metadata = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Json,
            original_byte_size: bytes.len() as u64,
            stored_byte_size: bytes.len() as u64,
            sha256: sha,
        };

        let put_result = AssertUnwindSafe(self.store.put(&key, &bytes, &metadata))
            .catch_unwind()
            .await;
        let outcome_ref = match put_result {
            Ok(Ok(outcome_ref)) => outcome_ref,
            Ok(Err(error)) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = %error,
                    "OutcomeStore::put failed; engaging truncation-ladder fallback"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
            Err(_) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = "panic in OutcomeStore::put",
                    "store backend panicked; engaging truncation-ladder fallback"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
        };

        let clipped_status = clip_status_strings(&result.status, self.status_string_cap_bytes);
        let clipped_diff = result
            .diff_summary
            .as_ref()
            .map(|diff| clip_diff_files(diff, self.diff_files_cap_count));
        let (clipped_summary, summary_clipped) =
            clip_with_ellipsis(result.summary.clone(), self.summary_cap_bytes);
        let (clipped_branch, _) =
            clip_with_ellipsis(result.worker_branch.clone(), self.worker_branch_cap_bytes);
        let clipped_artifact_ref = result
            .artifact
            .as_ref()
            .map(|artifact| build_artifact_ref(&delegation_id, artifact))
            .map(|artifact| {
                clip_artifact_ref_strings(&artifact, self.artifact_ref_string_cap_bytes)
            });

        let diff_files_clipped = matches!(
            (&result.diff_summary, &clipped_diff),
            (Some(original), Some(clipped)) if original.files.len() > clipped.files.len()
        );
        let hint = build_fetch_hint(summary_clipped, diff_files_clipped);
        let (fetch_hint, _) = clip_with_ellipsis(Some(hint), self.fetch_hint_cap_bytes);

        let base_hint = build_base_hint(clipped_branch.as_deref());
        let (base_hint, _) = clip_with_ellipsis(base_hint, self.base_hint_cap_bytes);

        let payload = ContinuationPayload {
            status: clipped_status,
            summary: clipped_summary,
            diff_summary: clipped_diff,
            worker_branch: clipped_branch,
            artifact_ref: clipped_artifact_ref,
            estimated_cost_micros: Some(usd_to_micros_saturating(result.estimated_cost_usd)),
            artifact_id: Some(key.clone()),
            fetch_hint,
            base_hint,
        };

        let cont = BrainContinuation {
            delegation_id: delegation_id.clone(),
            attempt,
            brain_session: brain_session.as_session_id().clone(),
            source: source.clone(),
            payload,
            created_at_wall: chrono::Utc::now(),
            created_at_mono: Instant::now(),
        };

        let envelope_bytes = estimate_envelope_cost(&cont.payload);
        debug_assert!(
            envelope_bytes <= spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
            "INV-D8 conservative estimate violation: {} > {} (post-clip)",
            envelope_bytes,
            spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES
        );
        if envelope_bytes > spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES {
            tracing::error!(
                target: "spur.metrics.materializer_oversized_post_clip",
                envelope_bytes,
                budget_bytes = spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
                ?key,
                "INV-D8 conservative estimate breached; engaging truncation-ladder fallback"
            );
            return self
                .fallback_truncation_ladder(
                    result,
                    delegation_id,
                    attempt,
                    brain_session,
                    source,
                    event_sink,
                )
                .await;
        }

        {
            let mut map = self
                .latest_attempt_by_delegation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            map.entry(delegation_id.clone())
                .and_modify(|current| *current = (*current).max(attempt))
                .or_insert(attempt);
        }

        tracing::info!(
            target: "spur.metrics.outcome_persisted",
            ?key,
            byte_size = outcome_ref.byte_size,
            sha256 = %outcome_ref.sha256,
            backend = ?outcome_ref.backend,
            latency_ms = start.elapsed().as_millis() as u64,
        );

        cont
    }

    async fn fallback_truncation_ladder(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        use spur_acp::domain::clip::{
            clip_artifact_ref_strings, clip_diff_files, clip_status_strings, clip_with_ellipsis,
        };
        use spur_acp::domain::events::SpurEventBody;
        use std::time::Instant;

        let start = Instant::now();

        let clipped_status = clip_status_strings(&result.status, self.status_string_cap_bytes);
        let original_summary_len = result.summary.as_ref().map(|s| s.len()).unwrap_or(0);
        let (clipped_summary, summary_truncated) =
            clip_with_ellipsis(result.summary.clone(), self.summary_cap_bytes);
        let (clipped_branch, _) =
            clip_with_ellipsis(result.worker_branch.clone(), self.worker_branch_cap_bytes);
        let clipped_diff = result
            .diff_summary
            .as_ref()
            .map(|diff| clip_diff_files(diff, self.diff_files_cap_count));
        let clipped_artifact_ref = result
            .artifact
            .as_ref()
            .map(|artifact| build_artifact_ref(&delegation_id, artifact))
            .map(|artifact| {
                clip_artifact_ref_strings(&artifact, self.artifact_ref_string_cap_bytes)
            });

        if summary_truncated {
            if let Some(sink) = event_sink {
                sink.emit(SpurEventBody::ContinuationFieldTruncated {
                    delegation_id: delegation_id.clone(),
                    field: "summary".into(),
                    original_bytes: original_summary_len,
                    kept_bytes: clipped_summary.as_ref().map(|s| s.len()).unwrap_or(0),
                });
            }
        }

        let payload = ContinuationPayload {
            status: clipped_status,
            summary: clipped_summary,
            diff_summary: clipped_diff,
            worker_branch: clipped_branch,
            artifact_ref: clipped_artifact_ref,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        };

        let mut cont = BrainContinuation {
            delegation_id: delegation_id.clone(),
            attempt,
            brain_session: brain_session.as_session_id().clone(),
            source,
            payload,
            created_at_wall: chrono::Utc::now(),
            created_at_mono: Instant::now(),
        };

        let mut envelope_bytes = estimate_envelope_cost(&cont.payload);
        let budget = spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES;

        // Ladder steps emit ContinuationFieldTruncated events as fields are
        // dropped so observability matches the actual on-wire state — silently
        // dropping fields would make the brain see "missing" data with no
        // operator signal that the materializer chose to drop them.
        let emit_drop =
            |sink: Option<&Arc<dyn McpEventSink>>, field: &'static str, original: usize| {
                if let Some(sink) = sink {
                    sink.emit(SpurEventBody::ContinuationFieldTruncated {
                        delegation_id: delegation_id.clone(),
                        field: field.into(),
                        original_bytes: original,
                        kept_bytes: 0,
                    });
                }
            };

        if envelope_bytes > budget {
            let original = cont.payload.summary.as_ref().map(|s| s.len()).unwrap_or(0);
            cont.payload.summary = None;
            emit_drop(event_sink, "summary", original);
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            let original = cont
                .payload
                .diff_summary
                .as_ref()
                .map(|d| d.files.len())
                .unwrap_or(0);
            cont.payload.diff_summary = None;
            emit_drop(event_sink, "diff_summary", original);
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            let original = cont
                .payload
                .artifact_ref
                .as_ref()
                .map(|a| a.uri.len())
                .unwrap_or(0);
            cont.payload.artifact_ref = None;
            emit_drop(event_sink, "artifact_ref", original);
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            cont.payload.status = clip_status_strings(&cont.payload.status, 128);
            emit_drop(event_sink, "status", self.status_string_cap_bytes);
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }

        if envelope_bytes > budget {
            tracing::error!(
                target: "spur.metrics.continuation_dropped_oversized",
                delegation_id = %delegation_id,
                envelope_bytes,
                budget_bytes = budget,
                "fallback ladder exhausted; emitting minimal continuation"
            );
            // Preserve the (clipped) status — overwriting with Success would
            // lie to the brain about a failed delegation. The clipped 128 B
            // status from the previous ladder step already fits the budget;
            // dropping the inline summary/diff/branch/artifact_ref recovers
            // additional headroom without misrepresenting outcome.
            cont.payload.summary = Some("(continuation oversized; fields dropped)".into());
            cont.payload.diff_summary = None;
            cont.payload.worker_branch = None;
            cont.payload.artifact_ref = None;
            emit_drop(event_sink, "worker_branch", 0);
        }

        tracing::warn!(
            target: "spur.metrics.outcome_persist_failed",
            delegation_id = %delegation_id,
            attempt,
            fallback_engaged = true,
            envelope_bytes,
            latency_ms = start.elapsed().as_millis() as u64,
        );
        cont
    }
}

/// Hint string surfaced to the brain when `artifact_id` is `Some(_)`.
/// Built from clipped status + diff so the brain knows which `section` to
/// fetch first. Capped at `fetch_hint_cap_bytes`.
pub(crate) fn build_fetch_hint(summary_clipped: bool, diff_files_clipped: bool) -> String {
    match (summary_clipped, diff_files_clipped) {
        (true, true) => {
            "Summary and diff truncated. Call fetch_outcome_artifact(delegation_id, section='full')."
                .to_string()
        }
        (false, true) => {
            "Diff file list truncated. Call fetch_outcome_artifact(delegation_id, section='diff_only')."
                .to_string()
        }
        (true, false) => {
            "Summary truncated. Call fetch_outcome_artifact(delegation_id, section='summary')."
                .to_string()
        }
        (false, false) => {
            "Full result available via fetch_outcome_artifact(delegation_id, section='full')."
                .to_string()
        }
    }
}

/// Contextual reminder for ad-hoc delegations. When a `worker_branch` is present,
/// tells the brain to pass that branch as `base` in follow-up `delegate_to_worker`
/// calls so the next worker sees prior context instead of defaulting to RepoMain.
/// Returns `None` when `worker_branch` is absent (plan-engine dispatches already
/// manage `WithOverlay` bases automatically).
pub(crate) fn build_base_hint(worker_branch: Option<&str>) -> Option<String> {
    worker_branch.map(|branch| {
        format!(
            "To delegate follow-up work on this result, pass base: {{\"kind\":\"branch\",\"name\":\"{}\"}}. Omitting base loses all prior worker context.",
            branch
        )
    })
}

fn sha256_hex(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    const CHARS: &[u8; 16] = b"0123456789abcdef";

    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(CHARS[(byte >> 4) as usize] as char);
        hex.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    hex
}

/// Convert `DelegationResult.estimated_cost_usd` to the v3 wire
/// representation `estimated_cost_micros`. Saturates at u64::MAX and clamps
/// negative, infinite, and NaN values to 0.
pub(crate) fn usd_to_micros_saturating(usd: f64) -> u64 {
    if !usd.is_finite() || usd < 0.0 {
        return 0;
    }
    let scaled = usd * 1_000_000.0;
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled.round() as u64
    }
}

fn build_artifact_ref(
    delegation_id: &DelegationId,
    artifact: &spur_acp::domain::artifact::WorkerArtifact,
) -> ArtifactRef {
    use spur_acp::domain::continuation::ArtifactKind;

    ArtifactRef {
        kind: ArtifactKind::Other("worker_artifact".into()),
        uri: format!("spur://artifact/{}", delegation_id.as_str()),
        byte_size: artifact.size_bytes as u64,
        sha256: Some(artifact.blob_sha.clone()),
        git_object_ref: Some(artifact.object_ref.clone()),
        git_blob_sha: Some(artifact.blob_sha.clone()),
    }
}

pub fn estimate_envelope_cost(payload: &ContinuationPayload) -> usize {
    use spur_acp::domain::merge_budget::ENVELOPE_WRAPPER_HEADROOM_BYTES;

    // Force the INV-D8 gate to fail (and thus engage the truncation-ladder
    // fallback) if the payload can't be serialized — silently returning 0
    // would let an unserializable payload sail past the budget check.
    let Ok(bytes) = serde_json::to_vec(payload) else {
        return usize::MAX;
    };
    bytes.len() + ENVELOPE_WRAPPER_HEADROOM_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::artifact::{ArtifactKind, WorkerArtifact};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::SessionId;
    use spur_blob_store::MemoryOutcomeStore;

    fn brain_session() -> BrainSessionId {
        BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()))
    }

    fn delegation_id() -> DelegationId {
        DelegationId::from("deadbeef-1111-2222-3333-444455556666")
    }

    fn small_result() -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: Some(WorkerArtifact {
                object_ref: "refs/spur/artifacts/test".into(),
                blob_sha: "0".repeat(40),
                size_bytes: 12,
                kind: ArtifactKind::Output,
            }),
        }
    }

    #[tokio::test]
    async fn materialize_success_populates_artifact_id() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let key = cont.payload.artifact_id.expect("artifact_id populated");
        assert_eq!(key.attempt, 1);
        assert_eq!(
            key.delegation_id.as_str(),
            "deadbeef-1111-2222-3333-444455556666"
        );
        assert_eq!(cont.payload.summary.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn materialize_clips_oversized_status_error() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let oversized = DelegationResult {
            status: DelegationStatus::Failed {
                error: "x".repeat(2000),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                oversized,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        if let DelegationStatus::Failed { error } = cont.payload.status {
            assert!(
                error.len() <= DEFAULT_STATUS_STRING_CAP_BYTES,
                "Failed.error must be clipped to status_string_cap_bytes"
            );
            assert!(error.ends_with('…'));
        } else {
            panic!("status variant changed");
        }
    }

    #[tokio::test]
    async fn materialize_persists_full_result_to_store() {
        use spur_blob_store::Section;

        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store.clone());
        let oversized_error = "z".repeat(5000);
        let oversized = DelegationResult {
            status: DelegationStatus::Failed {
                error: oversized_error.clone(),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                oversized,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let key = cont.payload.artifact_id.expect("artifact_id populated");
        let stored = store
            .get(&key, Some(Section::Full))
            .await
            .expect("persisted");
        let raw = String::from_utf8_lossy(&stored.bytes);
        assert!(
            raw.contains(&oversized_error),
            "stored blob must contain full unclipped error"
        );
    }

    #[tokio::test]
    async fn materialize_clips_diff_files_and_steers_fetch_hint() {
        use spur_acp::domain::events::DiffSummary;
        use std::path::PathBuf;

        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let many_files: Vec<PathBuf> = (0..64)
            .map(|i| PathBuf::from(format!("crates/foo/file_{i}.rs")))
            .collect();
        let diff = DiffSummary {
            files_changed: 64,
            insertions: 0,
            deletions: 0,
            files: many_files,
        };
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: Some(diff),
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                result,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let clipped = cont
            .payload
            .diff_summary
            .as_ref()
            .expect("diff_summary preserved");
        assert!(
            clipped.files.len() <= DEFAULT_DIFF_FILES_CAP_COUNT,
            "diff_summary.files must be capped at diff_files_cap_count"
        );
        let hint = cont.payload.fetch_hint.expect("fetch_hint populated");
        assert!(
            hint.contains("diff_only") || hint.contains("full"),
            "fetch_hint must steer the brain when diff was clipped (got: {hint})"
        );
    }

    #[tokio::test]
    async fn materialize_records_max_attempt_per_delegation() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let _ = mat
            .materialize(
                small_result(),
                delegation_id(),
                3,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        let _ = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        let latest = mat.latest_attempt(&delegation_id()).await;
        assert_eq!(
            latest,
            Some(3),
            "latest_attempt must hold max(seen), not last-seen"
        );
    }

    #[tokio::test]
    async fn materialize_falls_back_on_io_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};

        let store = MockFailingOutcomeStore::new(FailureMode::Io);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(
            cont.payload.artifact_id.is_none(),
            "fallback path must clear artifact_id"
        );
        assert!(cont.payload.fetch_hint.is_none());
        // Envelope (conservative estimate) must still fit. Use the same
        // helper the materializer uses internally; importing
        // `spur_core::continuation_bridge::continuation_cost_bytes` would
        // create a cycle for spur-mcp tests.
        let bytes = super::estimate_envelope_cost(&cont.payload);
        assert!(bytes <= spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES);
    }

    #[tokio::test]
    async fn materialize_falls_back_on_too_large_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};

        let store = MockFailingOutcomeStore::new(FailureMode::TooLarge);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_on_content_mismatch() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};

        let store = MockFailingOutcomeStore::new(FailureMode::ContentMismatch);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_on_backend_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};

        let store =
            MockFailingOutcomeStore::new(FailureMode::Backend("git update-ref failed".into()));
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_when_inner_store_panics() {
        // Spec §7.7 (Round 9 P3-S3) requires every FailureMode produce a
        // valid BrainContinuation — including a panicking backend. The
        // mock cannot panic-and-stay-testable inside an async fn (would
        // poison the runtime), so this test wires a one-off
        // `PanickingStore` inline and asserts the materializer collapses
        // into the truncation-ladder fallback via
        // `AssertUnwindSafe(store.put(...)).catch_unwind().await`.
        use async_trait::async_trait;
        use spur_acp::BrainSessionId;
        use spur_blob_store::{
            DeleteNamespaceReport, OutcomeContent, OutcomeKey as Key, OutcomeMetadata, OutcomeRef,
            OutcomeStore, Section, StoreError, SweepReport,
        };
        use std::sync::Arc;
        use std::time::Duration;

        struct PanickingStore;

        #[async_trait]
        impl OutcomeStore for PanickingStore {
            async fn put(
                &self,
                _key: &Key,
                _content: &[u8],
                _metadata: &OutcomeMetadata,
            ) -> Result<OutcomeRef, StoreError> {
                panic!("simulated backend panic");
            }

            async fn get(
                &self,
                _key: &Key,
                _section: Option<Section>,
            ) -> Result<OutcomeContent, StoreError> {
                Err(StoreError::Backend("unused".into()))
            }

            async fn delete_namespace(
                &self,
                _b: &BrainSessionId,
            ) -> Result<DeleteNamespaceReport, StoreError> {
                Ok(DeleteNamespaceReport::default())
            }

            async fn sweep_older_than(&self, _ttl: Duration) -> Result<SweepReport, StoreError> {
                Ok(SweepReport::default())
            }
        }

        let store: Arc<dyn OutcomeStore> = Arc::new(PanickingStore);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(
            cont.payload.artifact_id.is_none(),
            "panic in store.put must fall back, not unwind"
        );
    }

    #[tokio::test]
    async fn fallback_preserves_failed_status_at_last_resort() {
        // Last-resort branch must NOT overwrite status with Success when the
        // delegation actually failed. Test by forcing a Failed input through a
        // store that always returns TooLarge so the fallback path runs.
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};
        let store = MockFailingOutcomeStore::new(FailureMode::TooLarge);
        let mat = OutcomeMaterializer::new(store);
        let failed = DelegationResult {
            status: DelegationStatus::Failed {
                error: "compilation error".into(),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                failed,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(
            matches!(cont.payload.status, DelegationStatus::Failed { .. }),
            "fallback must preserve Failed status; got {:?}",
            cont.payload.status
        );
    }

    #[tokio::test]
    async fn materialize_populates_base_hint_when_worker_branch_present() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let mut result = small_result();
        result.worker_branch = Some("spur/worker-codex-deadbeef".into());
        let cont = mat
            .materialize(
                result,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::AsyncRequested,
                None,
            )
            .await;
        let hint = cont.payload.base_hint.expect("base_hint populated");
        assert!(
            hint.contains("spur/worker-codex-deadbeef"),
            "base_hint must name the worker_branch"
        );
        assert!(
            hint.contains("base"),
            "base_hint must mention the base parameter"
        );
    }

    #[tokio::test]
    async fn materialize_omits_base_hint_when_worker_branch_absent() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let mut result = small_result();
        result.worker_branch = None;
        let cont = mat
            .materialize(
                result,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::AsyncRequested,
                None,
            )
            .await;
        assert!(cont.payload.base_hint.is_none());
    }
}
