//! Verifies licensing events round-trip through serde JSON.

use std::collections::BTreeSet;

use chrono::{TimeZone, Utc};
use spur_acp::{
    LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent, LicenseSubjectKind,
    SpurEvent, SpurEventBody,
};

#[test]
fn license_updated_roundtrips() {
    let mut features = BTreeSet::new();
    features.insert("unlimited_agents".to_string());
    let ev = SpurEvent::now(SpurEventBody::LicenseUpdated {
        state: LicenseStateEvent {
            status: LicenseStatusEvent::Active,
            subject_kind: LicenseSubjectKind::Organization,
            plan: LicensePlan::Enterprise,
            features,
            expires_at: Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            binding_mode: LicenseBindingMode::Organization,
            offline_ok: true,
            status_text: "License validated".into(),
        },
    });

    let json = serde_json::to_string(&ev).expect("serialize");
    let round: SpurEvent = serde_json::from_str(&json).expect("deserialize");

    match round.body {
        SpurEventBody::LicenseUpdated { state } => {
            assert!(matches!(state.status, LicenseStatusEvent::Active));
            assert!(matches!(
                state.subject_kind,
                LicenseSubjectKind::Organization
            ));
            assert!(matches!(state.plan, LicensePlan::Enterprise));
            assert!(state.features.contains("unlimited_agents"));
            assert!(state.offline_ok);
            assert_eq!(state.status_text, "License validated");
        }
        other => panic!("expected LicenseUpdated, got {other:?}"),
    }
}
