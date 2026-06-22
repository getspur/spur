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
use spur_core::outcome_materializer::{estimate_envelope_cost, OutcomeMaterializer};

/// Bounded text up to `max_len` bytes, mixing ASCII with occasional
/// multi-byte UTF-8 codepoints so the materializer's `is_char_boundary`
/// logic in `clip_with_ellipsis` actually gets exercised.
///
/// `max_len` is the hard ceiling; the generator picks a random length
/// in `[0, max_len]` and assembles a string from a small alphabet that
/// includes 1- 2- and 4-byte UTF-8 chars. This is required for the
/// proptest to produce inputs that EXCEED the materializer's caps —
/// the previous regex-based version capped at 512 bytes regardless of
/// max_len, so clipping was never actually triggered.
fn arb_text(max_len: usize) -> impl Strategy<Value = String> {
    // Codepoints span byte widths so clipping has to handle UTF-8
    // boundaries. Includes ASCII, accented Latin (2-byte), CJK (3-byte),
    // and emoji (4-byte) for breadth.
    const ALPHABET: &[char] = &[
        'a', 'b', 'c', ' ', '/', '.', '_', '-', '0', '1', '9', '\n', 'é', 'ñ', '日', '本', '🦀',
        '✨',
    ];
    (0usize..=max_len)
        .prop_flat_map(move |target_bytes| {
            (
                Just(target_bytes),
                proptest::collection::vec(0usize..ALPHABET.len(), 0..=max_len),
            )
        })
        .prop_map(|(target_bytes, indices)| {
            let mut s = String::with_capacity(target_bytes);
            for i in indices {
                let ch = ALPHABET[i];
                if s.len() + ch.len_utf8() > target_bytes {
                    break;
                }
                s.push(ch);
            }
            s
        })
}

fn arb_path_string() -> impl Strategy<Value = String> {
    // Mix ASCII paths with multi-byte segments so clip_path_vec hits
    // is_char_boundary on the way down.
    proptest::collection::vec(
        prop_oneof![
            proptest::string::string_regex("[a-z/_.0-9]{1,64}").unwrap(),
            Just("crates/spur-mcp/é/file.rs".to_string()),
            Just("docs/超えて/path.md".to_string()),
            Just("logs/🦀-trace.txt".to_string()),
        ],
        1..=4,
    )
    .prop_map(|parts| parts.join("/"))
}

fn arb_diff_summary() -> impl Strategy<Value = DiffSummary> {
    (
        0usize..200,
        0usize..200_000,
        0usize..200_000,
        proptest::collection::vec(arb_path_string(), 0..40),
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

fn arb_timeout_fallback() -> impl Strategy<Value = TimeoutFallback> {
    prop_oneof![
        Just(TimeoutFallback::Approve),
        Just(TimeoutFallback::Abandon),
        arb_text(1_500).prop_map(|reason| TimeoutFallback::Reject { reason }),
    ]
}

fn arb_delegation_status() -> impl Strategy<Value = DelegationStatus> {
    prop_oneof![
        Just(DelegationStatus::Success),
        Just(DelegationStatus::Timeout),
        // Failed.error: drive past the 512-byte status cap so the clip
        // helper actually fires.
        arb_text(4_000).prop_map(|error| DelegationStatus::Failed { error }),
        // Conflict.files: large list of long paths to drive
        // clip_path_vec's count and per-path caps simultaneously.
        proptest::collection::vec(arb_path_string(), 0..50).prop_map(|files| {
            DelegationStatus::Conflict {
                files: files.into_iter().map(PathBuf::from).collect(),
            }
        }),
        arb_text(2_000).prop_map(|reason| DelegationStatus::Rejected { reason }),
        arb_text(2_000).prop_map(|reviewer_note| DelegationStatus::Modified { reviewer_note }),
        arb_text(2_000).prop_map(|reason| DelegationStatus::Cancelled { reason }),
        (
            any::<u64>().prop_map(Duration::from_secs),
            arb_timeout_fallback()
        )
            .prop_map(|(waited_for, fallback)| DelegationStatus::TimedOut {
                waited_for,
                fallback,
            },),
    ]
}

prop_compose! {
    /// summary: up to 4 KiB so the 512-byte materializer cap fires.
    /// diff: up to 32 KiB — never reaches the lean payload (the
    /// materializer doesn't inline diff text, only diff_summary), but
    /// kept for completeness of the full DelegationResult shape.
    /// worker_branch: up to 1 KiB so the 256-byte cap fires.
    fn arb_delegation_result()(
        status in arb_delegation_status(),
        summary in proptest::option::of(arb_text(4_000)),
        diff in proptest::option::of(arb_text(32_000)),
        diff_summary in proptest::option::of(arb_diff_summary()),
        worker_branch in proptest::option::of(arb_text(1_024)),
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
