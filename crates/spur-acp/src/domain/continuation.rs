use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::delegation::{DelegationId, DelegationStatus};
use crate::domain::events::DiffSummary;
use crate::types::SessionId;

fn created_at_mono_now() -> Instant {
    Instant::now()
}

/// Why SPUR is re-entering the brain with a continuation turn.
///
/// See `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContinuationSource {
    /// Originating call was `delegate_async`.
    AsyncRequested,
    /// `delegate_to_worker` exceeded the MCP block window; returned
    /// `delegation_id` for polling; worker later finished.
    BlockTimeout,
    /// Worker reached `DelegationStatus::Cancelled` (INV-6).
    Cancelled,
    /// `SpurEventBody::PlanTaskFailed` fired for a plan task the brain dispatched.
    PlanTaskFailed,
    /// `SpurEventBody::PlanTaskAwaitingReview` fired for a plan task the brain dispatched.
    PlanTaskAwaitingReview,
    /// `SpurEventBody::PlanCompleted` fired for a plan the brain dispatched.
    PlanCompleted,
    /// `SpurEventBody::PlanReadyToMerge` fired for a plan the brain dispatched.
    PlanReadyToMerge,
    /// bd-2m2u Phase 2d — a plan task exhausted its `AUTO_RETRY_BUDGET` and
    /// was promoted to `EscalatedToBrain`. The brain receives the continuation
    /// directly (option A) and resolves via `submit_plan_mutation`.
    PlanTaskEscalated,
}

/// Kind of side-channel continuation artifact the brain can fetch on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ArtifactKind {
    Patch,
    TestOutput,
    Log,
    Other(String),
}

/// Reference to a persisted continuation artifact.
///
/// **INVARIANT:** the `#[serde(flatten)]` attribute on `kind` is mandatory.
/// Removing it changes the wire shape from `{"kind":"patch","uri":...}` to
/// `{"kind":{"kind":"patch"},...}`. The golden round-trip test in
/// `crates/spur-acp/tests/artifact_ref_wire_compat.rs` enforces this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    #[serde(flatten)]
    pub kind: ArtifactKind,
    pub uri: String,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Git ref path (e.g., `"refs/spur/artifacts/<session>"`) when stored as a git blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_object_ref: Option<String>,
    /// 40-char hex SHA-1 of the git blob; survives ref deletion until git GC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_blob_sha: Option<String>,
}

/// Narrow projection of a worker outcome for scheduler consumption.
///
/// Deliberately NOT `DelegationResult` to decouple scheduler evolution
/// from result-struct evolution and to avoid moving large diffs through
/// the orchestrator ingress channel.
///
/// Schema v3 (Phase 3 of plan-5) adds `estimated_cost_micros`, `artifact_id`,
/// and `fetch_hint`. All three are additive `Option<>` with `#[serde(default,
/// skip_serializing_if)]` — wire-compatible with v2 producers/consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPayload {
    pub status: DelegationStatus,
    pub summary: Option<String>,
    pub diff_summary: Option<DiffSummary>,
    pub worker_branch: Option<String>,
    /// EXISTING (Phase 1 enriched) — reference to oversized stdout artifact (legacy narrow scope).
    /// Coexists with `artifact_id` during transition; deprecated after stabilization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ArtifactRef>,
    /// NEW (Phase 3 / MF3) — cost in micro-USD (1e-6 USD).
    /// `u64` chosen over `f64` so `ContinuationPayload` keeps deriving `Eq`
    /// (f64 does not impl Eq). See `estimated_cost_usd()` for display-time
    /// conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_micros: Option<u64>,
    /// NEW (Phase 3) — reference to the full delegation outcome in OutcomeStore.
    /// `Some(_)` ⇒ brain may call `fetch_outcome_artifact` for fuller context.
    /// Brains check `artifact_id` FIRST, fall back to `artifact_ref` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<crate::domain::outcome::OutcomeKey>,
    /// NEW (Phase 3) — explicit human-readable hint when `artifact_id` is `Some`.
    /// Capped at 256 B by the materializer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hint: Option<String>,
    /// NEW — contextual reminder for ad-hoc delegations. When `worker_branch` is
    /// present, this tells the brain to pass that branch as `base` in follow-up
    /// `delegate_to_worker` calls so the next worker sees prior context instead
    /// of defaulting to RepoMain. Capped at 256 B by the materializer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hint: Option<String>,
}

impl ContinuationPayload {
    /// Display-time conversion: micros → USD. Single canonical source so
    /// every consumer (TUI, dashboards, brain prompts) renders the same
    /// value. Returns `None` when `estimated_cost_micros` is `None`.
    ///
    /// Note: lossless up to `2^53` micros (~$9 billion); larger values
    /// truncate to the nearest representable `f64`. Practical API costs
    /// are many orders of magnitude below that ceiling.
    pub fn estimated_cost_usd(&self) -> Option<f64> {
        self.estimated_cost_micros.map(|m| m as f64 / 1_000_000.0)
    }
}

/// One detached delegation result awaiting brain re-entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainContinuation {
    /// Correlation key.
    pub delegation_id: DelegationId,
    /// Which retry attempt produced this. 1-based: attempt 1 is the first run.
    pub attempt: u32,
    /// Brain session this continuation targets.
    pub brain_session: SessionId,
    /// Why this continuation fired.
    pub source: ContinuationSource,
    /// Narrow projection of the worker outcome.
    pub payload: ContinuationPayload,
    /// Wall-clock at producer, used on the wire for brain-visible recency.
    pub created_at_wall: DateTime<Utc>,
    /// Process-local monotonic timestamp for scheduler ordering only.
    ///
    /// Inbound deserialize is not expected on current code paths; if it does
    /// happen, a fresh `Instant::now()` is synthesized because monotonic clock
    /// values are not portable across processes.
    #[serde(skip, default = "created_at_mono_now")]
    pub created_at_mono: Instant,
}

/// Delivery dedup key scoped to one delegation attempt.
#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub struct DelegationKey {
    pub delegation_id: DelegationId,
    pub attempt: u32,
}

impl From<&BrainContinuation> for DelegationKey {
    fn from(value: &BrainContinuation) -> Self {
        Self {
            delegation_id: value.delegation_id.clone(),
            attempt: value.attempt,
        }
    }
}

/// TERMINAL: the continuation will not be retried or delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DropReason {
    SessionSwap,
    StaleSession,
    AlreadyDelivered,
    OverflowFull,
    OverflowChannelClosed,
    OversizedSingleItem {
        continuation_bytes: usize,
        budget_bytes: usize,
    },
    MaxRequeueExceeded,
    MismatchedCommitKeys,
    RequeueChannelFull,
    RetrySuperseded,
}

/// RETRIABLE: the continuation returns to pending for a future turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DeferReason {
    BudgetSpill {
        budget_bytes: usize,
        continuation_bytes: usize,
    },
    PromptDispatchFailure,
    LeakedBatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use std::collections::HashSet;
    use std::time::Duration;

    use crate::types::SessionId;

    #[test]
    fn continuation_payload_builds_from_parts() {
        let p = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("done".into()),
            diff_summary: None,
            worker_branch: Some("wt/abc".into()),
            artifact_ref: Some(ArtifactRef {
                kind: ArtifactKind::Patch,
                uri: "spur://artifact/abc".into(),
                byte_size: 42,
                sha256: Some("a".repeat(64)),
                git_object_ref: None,
                git_blob_sha: None,
            }),
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        };
        assert_eq!(p.summary.as_deref(), Some("done"));
        assert!(matches!(p.status, DelegationStatus::Success));
        assert_eq!(
            p.artifact_ref.as_ref().map(|artifact| artifact.byte_size),
            Some(42)
        );
    }

    #[test]
    fn continuation_payload_v3_round_trips_through_serde() {
        use crate::domain::outcome::OutcomeKey;
        use crate::types::SessionId;
        use crate::BrainSessionId;

        let payload = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("ok".into()),
            diff_summary: None,
            worker_branch: Some("spur/worker-x".into()),
            artifact_ref: None,
            estimated_cost_micros: Some(12_345),
            artifact_id: Some(OutcomeKey {
                brain_session_id: BrainSessionId::new(SessionId(
                    "550e8400-e29b-41d4-a716-446655440000".into(),
                )),
                delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
                attempt: 1,
            }),
            fetch_hint: Some("Full diff truncated. Call fetch_outcome_artifact.".into()),
            base_hint: None,
        };
        let s = serde_json::to_string(&payload).expect("serialize");
        let back: ContinuationPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, payload);
    }

    #[test]
    fn estimated_cost_usd_converts_micros_correctly() {
        let payload = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: None,
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: Some(1_234_567),
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        };
        let usd = payload.estimated_cost_usd().expect("Some");
        assert!((usd - 1.234567).abs() < 1e-9);
    }

    #[test]
    fn estimated_cost_usd_handles_none_and_zero_and_large_values() {
        let none_payload = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: None,
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
            base_hint: None,
        };
        assert!(none_payload.estimated_cost_usd().is_none());

        let zero_payload = ContinuationPayload {
            estimated_cost_micros: Some(0),
            ..none_payload.clone()
        };
        assert_eq!(zero_payload.estimated_cost_usd(), Some(0.0));

        // Past 2^53: precision is lost but the conversion still produces a
        // finite USD value (the docstring notes this is out of practical range).
        let big = (1u64 << 53) + 1;
        let big_payload = ContinuationPayload {
            estimated_cost_micros: Some(big),
            ..none_payload
        };
        let usd = big_payload.estimated_cost_usd().expect("Some");
        assert!(usd.is_finite());
        assert!(usd > 0.0);
    }

    #[test]
    fn v3_payload_deserializes_from_v2_envelope_with_serde_default() {
        let v2_json = r#"{
            "status": "Success",
            "summary": null,
            "diff_summary": null,
            "worker_branch": null
        }"#;
        let back: ContinuationPayload = serde_json::from_str(v2_json).expect("deserialize");
        assert!(back.estimated_cost_micros.is_none());
        assert!(back.artifact_id.is_none());
        assert!(back.fetch_hint.is_none());
        assert!(back.base_hint.is_none());
    }

    #[test]
    fn continuation_source_round_trips_as_tagged_snake_case() {
        let source = ContinuationSource::AsyncRequested;
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json, json!({ "kind": "async_requested" }));

        let back: ContinuationSource = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContinuationSource::AsyncRequested));
    }

    #[test]
    fn brain_continuation_round_trips_wire_fields_and_refreshes_created_at_mono() {
        let c = BrainContinuation {
            delegation_id: "uuid-1".into(),
            attempt: 1,
            brain_session: SessionId("brain-session-1".into()),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: Some("done".into()),
                diff_summary: None,
                worker_branch: Some("wt/abc".into()),
                artifact_ref: Some(ArtifactRef {
                    kind: ArtifactKind::Patch,
                    uri: "spur://artifact/abc".into(),
                    byte_size: 42,
                    sha256: Some("f".repeat(64)),
                    git_object_ref: None,
                    git_blob_sha: None,
                }),
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
            },
            created_at_wall: Utc.with_ymd_and_hms(2026, 4, 24, 12, 34, 56).unwrap(),
            created_at_mono: Instant::now(),
        };

        let serialized = serde_json::to_string(&c).unwrap();
        assert!(serialized.contains("\"created_at_wall\""));
        assert!(!serialized.contains("created_at_mono"));

        let before = Instant::now();
        let back: BrainContinuation = serde_json::from_str(&serialized).unwrap();
        let after = Instant::now();

        assert_eq!(back.delegation_id, c.delegation_id);
        assert_eq!(back.attempt, c.attempt);
        assert_eq!(back.brain_session, c.brain_session);
        assert!(matches!(back.source, ContinuationSource::AsyncRequested));
        assert_eq!(back.created_at_wall, c.created_at_wall);
        assert_eq!(back.payload.summary, c.payload.summary);
        assert_eq!(back.payload.worker_branch, c.payload.worker_branch);
        assert_eq!(
            back.payload
                .artifact_ref
                .as_ref()
                .map(|artifact| artifact.byte_size),
            Some(42)
        );
        assert!(back.created_at_mono >= before);
        assert!(back.created_at_mono <= after);
    }

    #[test]
    fn delegation_key_equality_and_hashing_use_attempt() {
        let first = BrainContinuation {
            delegation_id: "uuid-1".into(),
            attempt: 1,
            brain_session: SessionId("brain-session-1".into()),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
                artifact_ref: None,
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
            },
            created_at_wall: Utc.with_ymd_and_hms(2026, 4, 24, 12, 34, 56).unwrap(),
            created_at_mono: Instant::now(),
        };
        let same = BrainContinuation {
            attempt: 1,
            created_at_mono: Instant::now() + Duration::from_millis(1),
            ..first.clone()
        };
        let next_attempt = BrainContinuation {
            attempt: 2,
            ..first.clone()
        };

        let first_key = DelegationKey::from(&first);
        let same_key = DelegationKey::from(&same);
        let next_key = DelegationKey::from(&next_attempt);

        assert_eq!(first_key, same_key);
        assert_ne!(first_key, next_key);

        let keys = HashSet::from([first_key, same_key, next_key]);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn drop_and_defer_reasons_round_trip_through_tagged_serde() {
        let drop_reason = DropReason::OversizedSingleItem {
            continuation_bytes: 8192,
            budget_bytes: 4096,
        };
        let drop_json = serde_json::to_value(&drop_reason).unwrap();
        assert_eq!(
            drop_json,
            json!({
                "reason": "oversized_single_item",
                "continuation_bytes": 8192,
                "budget_bytes": 4096
            })
        );
        let back_drop: DropReason = serde_json::from_value(drop_json).unwrap();
        assert!(matches!(
            back_drop,
            DropReason::OversizedSingleItem {
                continuation_bytes: 8192,
                budget_bytes: 4096
            }
        ));

        let defer_reason = DeferReason::BudgetSpill {
            budget_bytes: 4096,
            continuation_bytes: 1024,
        };
        let defer_json = serde_json::to_value(&defer_reason).unwrap();
        assert_eq!(
            defer_json,
            json!({
                "reason": "budget_spill",
                "budget_bytes": 4096,
                "continuation_bytes": 1024
            })
        );
        let back_defer: DeferReason = serde_json::from_value(defer_json).unwrap();
        assert!(matches!(
            back_defer,
            DeferReason::BudgetSpill {
                budget_bytes: 4096,
                continuation_bytes: 1024
            }
        ));
    }
}
