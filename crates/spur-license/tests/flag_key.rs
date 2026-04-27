use spur_license::policy::{FlagSpec, PolicyDocument};
use spur_license::{
    FlagKey, ENABLE_BROWSER_TOOL, ENABLE_COMPACTION_V2, ENABLE_TELEMETRY, KILL_ADVANCED_PLANNER,
};

#[test]
fn known_flags_exist_in_typed_registry() {
    assert_eq!(KILL_ADVANCED_PLANNER.as_str(), "kill_advanced_planner");
    assert_eq!(ENABLE_BROWSER_TOOL.as_str(), "enable_browser_tool");
    assert_eq!(ENABLE_COMPACTION_V2.as_str(), "enable_compaction_v2");
    assert_eq!(ENABLE_TELEMETRY.as_str(), "enable_telemetry");

    assert_eq!(
        FlagKey::from_known("kill_advanced_planner"),
        Some(KILL_ADVANCED_PLANNER)
    );
    assert_eq!(
        FlagKey::from_known("enable_browser_tool"),
        Some(ENABLE_BROWSER_TOOL)
    );
    assert_eq!(
        FlagKey::from_known("enable_compaction_v2"),
        Some(ENABLE_COMPACTION_V2)
    );
    assert_eq!(
        FlagKey::from_known("enable_telemetry"),
        Some(ENABLE_TELEMETRY)
    );
    assert_eq!(FlagKey::from_known("not_a_flag"), None);
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
        doc.flags.get(&KILL_ADVANCED_PLANNER),
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
