//! Verifies that retry workers see the accumulated history of
//! previous attempts (summary, diff stats, reviewer feedback).
//!
//! Exercises render_retry_context directly — the integration path
//! from execute_delegation into the worker prompt is covered by
//! the inline unit tests in orchestrator.rs.

use spur_acp::DiffSummary;
use spur_core::test_support::{render_retry_context_public, RetryAttemptPublic};
use std::path::PathBuf;

#[test]
fn third_attempt_prompt_contains_both_prior_attempts() {
    let history = vec![
        RetryAttemptPublic {
            attempt_n: 1,
            summary: "I added rate limiting as a fixed window".into(),
            diff_summary: Some(DiffSummary {
                files_changed: 2,
                insertions: 40,
                deletions: 3,
                files: vec![PathBuf::from("src/rl.rs"), PathBuf::from("src/lib.rs")],
            }),
            feedback: "prefer token bucket".into(),
        },
        RetryAttemptPublic {
            attempt_n: 2,
            summary: "Switched to token bucket with hardcoded limits".into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 22,
                deletions: 8,
                files: vec![PathBuf::from("src/rl.rs")],
            }),
            feedback: "make the bucket size configurable per endpoint".into(),
        },
    ];
    let prompt = render_retry_context_public(
        &history,
        "Add rate limiting middleware",
        "make the bucket size configurable per endpoint",
    );

    // Both prior attempts' summaries must appear.
    assert!(prompt.contains("I added rate limiting as a fixed window"));
    assert!(prompt.contains("Switched to token bucket"));
    // Reviewer feedback from each attempt must appear.
    assert!(prompt.contains("prefer token bucket"));
    assert!(prompt.contains("configurable per endpoint"));
    // Diff stats render.
    assert!(prompt.contains("2 changed"));
    assert!(prompt.contains("+40"));
    // Original task is preserved.
    assert!(prompt.starts_with("Add rate limiting middleware"));
}
