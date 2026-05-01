use spur_acp::{AttemptSetupError, DelegationStatus, TimeoutFallback};
use std::time::Duration;

fn roundtrip(status: &DelegationStatus) {
    let json = serde_json::to_string(status).expect("serialize");
    let back: DelegationStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(*status, back, "value mismatch after deserialization");
    let json2 = serde_json::to_string(&back).expect("re-serialize");
    assert_eq!(json, json2, "round-trip mismatch");
}

#[test]
fn every_variant_round_trips() {
    roundtrip(&DelegationStatus::Success);
    roundtrip(&DelegationStatus::Failed {
        error: "boom".into(),
    });
    roundtrip(&DelegationStatus::Conflict { files: vec![] });
    roundtrip(&DelegationStatus::Timeout);
    roundtrip(&DelegationStatus::Rejected {
        reason: "too large".into(),
    });
    roundtrip(&DelegationStatus::Modified {
        reviewer_note: "fix naming".into(),
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Approve,
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    });
    roundtrip(&DelegationStatus::SetupFailed {
        error: AttemptSetupError::OverlayConflict {
            source_task_id: "T1".into(),
            files: vec!["foo.rs".into()],
        },
    });
}

#[test]
fn rejected_is_distinguishable_from_timed_out_reject() {
    let human = DelegationStatus::Rejected {
        reason: "refactor this".into(),
    };
    let system = DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    };
    let j_human = serde_json::to_value(&human).unwrap();
    let j_system = serde_json::to_value(&system).unwrap();
    assert_ne!(j_human, j_system);
    assert!(j_human.to_string().contains("Rejected"));
    assert!(j_system.to_string().contains("TimedOut"));
}
