use spur_license::policy::feature_key::FeatureKey;
use std::collections::HashSet;

#[test]
fn known_features_exist() {
    assert_eq!(FeatureKey::CHAT.as_str(), "chat");
    assert_eq!(FeatureKey::ADVANCED_AGENTS.as_str(), "advanced_agents");
    assert_eq!(FeatureKey::KILL_ADVANCED_PLANNER.as_str(), "kill_advanced_planner");
}

#[test]
fn from_known_parses_all_keys() {
    // Spot-check a few from each tier
    assert_eq!(FeatureKey::from_known("chat"), Some(FeatureKey::CHAT));
    assert_eq!(FeatureKey::from_known("cloud_sync"), Some(FeatureKey::CLOUD_SYNC));
    assert_eq!(FeatureKey::from_known("audit_logs"), Some(FeatureKey::AUDIT_LOGS));
    assert_eq!(
        FeatureKey::from_known("dedicated_hosting"),
        Some(FeatureKey::DEDICATED_HOSTING)
    );
    assert_eq!(
        FeatureKey::from_known("enable_browser_tool"),
        Some(FeatureKey::ENABLE_BROWSER_TOOL)
    );

    // Unknown strings must return None
    assert_eq!(FeatureKey::from_known("nonexistent_feature"), None);
    assert_eq!(FeatureKey::from_known(""), None);
}

#[test]
fn feature_key_is_copy_and_hashable() {
    let a = FeatureKey::CHAT;
    let b = FeatureKey::CHAT;
    assert_eq!(a, b);

    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));

    // Verify Copy by using after move into HashSet
    let _ = a;
}
