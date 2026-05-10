use std::path::PathBuf;
use std::time::Instant;

use chrono::{TimeZone, Utc};
use spur_acp::domain::events::DiffSummary;
use spur_acp::domain::outcome::OutcomeKey;
use spur_acp::domain::{
    ArtifactRef, BrainContinuation, ContinuationPayload, ContinuationSource, DelegationId,
    DelegationStatus,
};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::continuation_bridge::{continuation_cost_bytes, MERGE_BUDGET_DEFAULT_BYTES};
use spur_mcp::outcome_materializer::estimate_envelope_cost;

fn outcome_key(delegation_id: DelegationId, attempt: u32) -> OutcomeKey {
    OutcomeKey {
        brain_session_id: BrainSessionId::new(SessionId(
            "550e8400-e29b-41d4-a716-446655440000".into(),
        )),
        delegation_id,
        attempt,
    }
}

fn continuation(
    id: &str,
    attempt: u32,
    source: ContinuationSource,
    payload: ContinuationPayload,
) -> BrainContinuation {
    BrainContinuation {
        delegation_id: DelegationId::from(id),
        attempt,
        brain_session: SessionId("550e8400-e29b-41d4-a716-446655440000".into()),
        source,
        payload,
        created_at_wall: Utc.with_ymd_and_hms(2026, 4, 25, 12, 0, 0).unwrap(),
        created_at_mono: Instant::now(),
    }
}

fn success_payload(id: &str, attempt: u32, summary: &str) -> ContinuationPayload {
    ContinuationPayload {
        status: DelegationStatus::Success,
        summary: Some(summary.into()),
        diff_summary: None,
        worker_branch: Some("spur/worker-x".into()),
        artifact_ref: None,
        estimated_cost_micros: Some(42),
        artifact_id: Some(outcome_key(DelegationId::from(id), attempt)),
        fetch_hint: Some(
            "Full result available via fetch_outcome_artifact(delegation_id, section='full')."
                .into(),
        ),
        base_hint: None,
    }
}

#[test]
fn conservative_estimate_dominates_exact_cost() {
    let small = continuation(
        "deadbeef-1111-2222-3333-444455556666",
        1,
        ContinuationSource::BlockTimeout,
        success_payload("deadbeef-1111-2222-3333-444455556666", 1, "done"),
    );

    let mid = continuation(
        "deadbeef-1111-2222-3333-444455556667",
        2,
        ContinuationSource::PlanCompleted,
        ContinuationPayload {
            diff_summary: Some(DiffSummary {
                files_changed: 8,
                insertions: 120,
                deletions: 24,
                files: (0..8)
                    .map(|i| PathBuf::from(format!("crates/spur-core/src/file_{i}.rs")))
                    .collect(),
            }),
            ..success_payload(
                "deadbeef-1111-2222-3333-444455556667",
                2,
                &"summary ".repeat(60),
            )
        },
    );

    let large = continuation(
        "deadbeef-1111-2222-3333-444455556668",
        3,
        ContinuationSource::PlanReadyToMerge,
        ContinuationPayload {
            status: DelegationStatus::Failed {
                error: "x".repeat(512),
            },
            summary: Some("z".repeat(512)),
            diff_summary: Some(DiffSummary {
                files_changed: 16,
                insertions: 4096,
                deletions: 1024,
                files: (0..16)
                    .map(|i| PathBuf::from(format!("crates/spur-mcp/src/long_file_name_{i}.rs")))
                    .collect(),
            }),
            artifact_ref: Some(ArtifactRef {
                kind: spur_acp::domain::continuation::ArtifactKind::Other("worker_artifact".into()),
                uri: "spur://artifact/deadbeef-1111-2222-3333-444455556668".into(),
                byte_size: 5000,
                sha256: Some("a".repeat(64)),
                git_object_ref: Some("refs/spur/artifacts/test".into()),
                git_blob_sha: Some("b".repeat(40)),
            }),
            ..success_payload("deadbeef-1111-2222-3333-444455556668", 3, "")
        },
    );

    for continuation in [small, mid, large] {
        let estimate = estimate_envelope_cost(&continuation.payload);
        let exact = continuation_cost_bytes(&continuation);

        assert!(
            estimate >= exact,
            "estimate {estimate} must dominate exact rendered cost {exact}"
        );
        assert!(
            estimate <= MERGE_BUDGET_DEFAULT_BYTES,
            "representative materialized continuation estimate {estimate} exceeds budget"
        );
    }
}
