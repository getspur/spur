//! INV-D9 proptest: every `DelegationStatus` variant, when run through
//! `OutcomeMaterializer::materialize`, produces a `BrainContinuation`
//! whose envelope (conservative estimate) fits under
//! `MERGE_BUDGET_DEFAULT_BYTES`.
//!
//! Generalizes the existing unit tests
//! (`materialize_clips_oversized_status_error`, etc.) to exhaustive
//! variant coverage. Round 9 P3-S2: arb generators broadened so every
//! status produces a continuation under budget.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use spur_acp::domain::{
    delegation::{DelegationStatus, TimeoutFallback},
    events::DiffSummary,
    merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
    ContinuationSource, DelegationId, DelegationResult,
};
use spur_acp::{BrainSessionId, SessionId};
use spur_blob_store::{MemoryOutcomeStore, OutcomeStore};
use spur_mcp::outcome_materializer::{estimate_envelope_cost, OutcomeMaterializer};

prop_compose! {
    /// Bounded printable string so the generator doesn't randomly exceed
    /// the budget at the input layer; this targets materializer clipping.
    fn arb_text(max_len: usize)(s in proptest::string::string_regex("[a-z ]{0,512}").unwrap())
        -> String { s.chars().take(max_len).collect() }
}

fn arb_diff_summary() -> impl Strategy<Value = DiffSummary> {
    (
        0usize..200,
        0usize..200_000,
        0usize..200_000,
        proptest::collection::vec(
            proptest::string::string_regex("[a-z/_.0-9]{1,64}").unwrap(),
            0..40,
        ),
    )
        .prop_map(
            |(files_changed, insertions, deletions, files)| DiffSummary {
                files_changed,
                insertions,
                deletions,
                files: files.into_iter().map(PathBuf::from).collect(),
            },
        )
}

fn arb_delegation_status() -> impl Strategy<Value = DelegationStatus> {
    prop_oneof![
        Just(DelegationStatus::Success),
        Just(DelegationStatus::Timeout),
        arb_text(2_000).prop_map(|error| DelegationStatus::Failed { error }),
        proptest::collection::vec(
            proptest::string::string_regex("[a-z/_.0-9]{1,128}").unwrap(),
            0..40,
        )
        .prop_map(|files| DelegationStatus::Conflict {
            files: files.into_iter().map(PathBuf::from).collect(),
        }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::Rejected { reason }),
        arb_text(1_000).prop_map(|reviewer_note| DelegationStatus::Modified { reviewer_note }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::Cancelled { reason }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::TimedOut {
            waited_for: Duration::from_secs(60),
            fallback: TimeoutFallback::Reject { reason },
        }),
    ]
}

prop_compose! {
    fn arb_delegation_result()(
        status in arb_delegation_status(),
        summary in proptest::option::of(arb_text(2_000)),
        diff in proptest::option::of(arb_text(20_000)),
        diff_summary in proptest::option::of(arb_diff_summary()),
        worker_branch in proptest::option::of(arb_text(512)),
        cost_micros in 0u64..1_000_000_000,
    ) -> DelegationResult {
        DelegationResult {
            status,
            diff,
            diff_summary,
            summary,
            estimated_cost_usd: cost_micros as f64 / 1_000_000.0,
            worker_branch,
            artifact: None,
        }
    }
}

fn brain_session() -> BrainSessionId {
    BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn inv_d9_arb_delegation_status_clips_under_budget(result in arb_delegation_result()) {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let cont = futures::executor::block_on(mat.materialize(
            result,
            DelegationId::from("deadbeef-1111-2222-3333-444455556666"),
            1,
            brain_session(),
            ContinuationSource::BlockTimeout,
            None,
        ));
        let envelope = estimate_envelope_cost(&cont.payload);
        prop_assert!(
            envelope <= MERGE_BUDGET_DEFAULT_BYTES,
            "INV-D9 violation: envelope={envelope} > budget={}",
            MERGE_BUDGET_DEFAULT_BYTES,
        );
    }
}
