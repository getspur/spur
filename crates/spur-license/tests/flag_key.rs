use spur_license::policy::{FlagSpec, PolicyDocument};
use spur_license::FlagKey;

#[test]
fn known_flags_exist_in_typed_registry() {
    assert_eq!(
        FlagKey::KILL_ADVANCED_PLANNER.as_str(),
        "kill_advanced_planner"
    );
    assert_eq!(FlagKey::ENABLE_BROWSER_TOOL.as_str(), "enable_browser_tool");
    assert_eq!(
        FlagKey::ENABLE_COMPACTION_V2.as_str(),
        "enable_compaction_v2"
    );
    assert_eq!(FlagKey::ENABLE_TELEMETRY.as_str(), "enable_telemetry");
    assert_eq!(FlagKey::ENABLE_V1_1_PREVIEW.as_str(), "enable_v1_1_preview");

    assert_eq!(
        FlagKey::from_known("kill_advanced_planner"),
        Some(FlagKey::KILL_ADVANCED_PLANNER)
    );
    assert_eq!(
        FlagKey::from_known("enable_browser_tool"),
        Some(FlagKey::ENABLE_BROWSER_TOOL)
    );
    assert_eq!(
        FlagKey::from_known("enable_compaction_v2"),
        Some(FlagKey::ENABLE_COMPACTION_V2)
    );
    assert_eq!(
        FlagKey::from_known("enable_telemetry"),
        Some(FlagKey::ENABLE_TELEMETRY)
    );
    assert_eq!(
        FlagKey::from_known("enable_v1_1_preview"),
        Some(FlagKey::ENABLE_V1_1_PREVIEW)
    );
    assert_eq!(FlagKey::from_known("not_a_flag"), None);
}

#[test]
fn from_known_is_const_callable() {
    const KILL: Option<FlagKey> = FlagKey::from_known("kill_advanced_planner");
    const UNKNOWN: Option<FlagKey> = FlagKey::from_known("not_a_flag");

    assert_eq!(KILL, Some(FlagKey::KILL_ADVANCED_PLANNER));
    assert_eq!(UNKNOWN, None);
}

#[test]
fn registered_flag_count_matches_expected() {
    const EXPECTED_TOTAL_KEYS: usize = 5;
    let mut count = 0usize;
    for key in [
        FlagKey::KILL_ADVANCED_PLANNER,
        FlagKey::ENABLE_BROWSER_TOOL,
        FlagKey::ENABLE_COMPACTION_V2,
        FlagKey::ENABLE_TELEMETRY,
        FlagKey::ENABLE_V1_1_PREVIEW,
    ] {
        assert_eq!(FlagKey::from_known(key.as_str()), Some(key));
        count += 1;
    }
    assert_eq!(count, EXPECTED_TOTAL_KEYS);
}

#[test]
fn unknown_policy_flags_are_dropped_during_parse() {
    let json = r#"{
        "schema_version": 2,
        "issued_at": "2026-04-27T00:00:00Z",
        "policy_version": "2026-04-27",
        "tier_policies": {},
        "flags": {
            "kill_advanced_planner": {"enabled": false},
            "unknown_future_flag": {"enabled": true}
        }
    }"#;
    let doc: PolicyDocument = serde_json::from_str(json).unwrap();

    assert_eq!(doc.flags.len(), 1);
    assert_eq!(
        doc.flags.get(&FlagKey::KILL_ADVANCED_PLANNER),
        Some(&FlagSpec {
            enabled: false,
            ..FlagSpec::default()
        })
    );
    assert!(doc
        .flags
        .keys()
        .all(|key| key.as_str() != "unknown_future_flag"));
}
