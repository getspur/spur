use spur_acp::{DelegationResult, DelegationStatus, TimeoutFallback};
use std::path::PathBuf;
use std::time::Duration;

fn render(r: &DelegationResult) -> String {
    serde_json::to_string_pretty(r).expect("serialize")
}

fn base(status: DelegationStatus) -> DelegationResult {
    DelegationResult {
        status,
        diff: None,
        diff_summary: None,
        summary: Some("s".into()),
        estimated_cost_usd: 0.0,
    }
}

#[test]
fn each_review_variant_renders_distinctly() {
    let success = render(&base(DelegationStatus::Success));
    let rejected = render(&base(DelegationStatus::Rejected {
        reason: "too large".into(),
    }));
    let modified = render(&base(DelegationStatus::Modified {
        reviewer_note: "fix naming".into(),
    }));
    let timed_out = render(&base(DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    }));
    let failed = render(&base(DelegationStatus::Failed {
        error: "worker errored".into(),
    }));
    let conflict = render(&base(DelegationStatus::Conflict {
        files: vec![PathBuf::from("a/b.rs")],
    }));
    let timeout = render(&base(DelegationStatus::Timeout));

    // Discriminator strings must be present so the brain's prompt can
    // pattern-match on them.
    assert!(success.contains("\"Success\""));
    assert!(rejected.contains("\"Rejected\""));
    assert!(rejected.contains("too large"));
    assert!(modified.contains("\"Modified\""));
    assert!(modified.contains("fix naming"));
    assert!(timed_out.contains("\"TimedOut\""));
    assert!(timed_out.contains("review timeout"));
    assert!(failed.contains("\"Failed\""));
    assert!(failed.contains("worker errored"));
    assert!(conflict.contains("\"Conflict\""));
    assert!(timeout.contains("\"Timeout\""));

    // Mutual distinguishability — brain must be able to tell them apart.
    let renders = [
        &success, &rejected, &modified, &timed_out, &failed, &conflict, &timeout,
    ];
    for (i, a) in renders.iter().enumerate() {
        for (j, b) in renders.iter().enumerate() {
            if i != j {
                assert_ne!(
                    a, b,
                    "variants at idx {} and {} must render distinctly",
                    i, j
                );
            }
        }
    }
}

#[test]
fn timed_out_fallback_discriminants_are_distinct() {
    let reject = render(&base(DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Reject { reason: "r".into() },
    }));
    let approve = render(&base(DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Approve,
    }));
    let abandon = render(&base(DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    }));
    assert_ne!(reject, approve);
    assert_ne!(reject, abandon);
    assert_ne!(approve, abandon);
    assert!(reject.contains("\"Reject\""));
    assert!(approve.contains("\"Approve\""));
    assert!(abandon.contains("\"Abandon\""));
}

#[test]
fn human_rejected_distinct_from_timed_out_reject_fallback() {
    // The key spec invariant: a human-issued Rejected must be
    // distinguishable from a system-applied TimedOut(Reject) so the
    // brain doesn't treat a timeout as actionable feedback.
    let human = render(&base(DelegationStatus::Rejected {
        reason: "refactor this".into(),
    }));
    let system = render(&base(DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    }));
    assert_ne!(human, system);
    // The discriminator strings should be present, not just the reason.
    assert!(human.contains("\"Rejected\""));
    assert!(!human.contains("\"TimedOut\""));
    assert!(system.contains("\"TimedOut\""));
    assert!(!system.contains("\"Rejected\""));
}
