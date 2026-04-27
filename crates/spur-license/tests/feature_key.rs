use spur_license::policy::feature_key::FeatureKey;
use std::collections::HashSet;

#[test]
fn known_features_exist() {
    assert_eq!(FeatureKey::BRAIN_SESSION.as_str(), "brain_session");
    assert_eq!(FeatureKey::PARALLEL_WORKERS.as_str(), "parallel_workers");
}

#[test]
fn from_known_parses_all_keys() {
    // Spot-check a few from each tier
    assert_eq!(
        FeatureKey::from_known("brain_session"),
        Some(FeatureKey::BRAIN_SESSION)
    );
    assert_eq!(
        FeatureKey::from_known("auto_review_policies"),
        Some(FeatureKey::AUTO_REVIEW_POLICIES)
    );
    assert_eq!(FeatureKey::from_known("rbac"), Some(FeatureKey::RBAC));
    assert_eq!(
        FeatureKey::from_known("dedicated_support"),
        Some(FeatureKey::DEDICATED_SUPPORT)
    );
    // Unknown strings must return None
    assert_eq!(FeatureKey::from_known("nonexistent_feature"), None);
    assert_eq!(FeatureKey::from_known("enable_browser_tool"), None);
    assert_eq!(FeatureKey::from_known(""), None);
}

#[test]
fn feature_key_is_copy_and_hashable() {
    let a = FeatureKey::BRAIN_SESSION;
    let b = FeatureKey::BRAIN_SESSION;
    assert_eq!(a, b);

    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));

    // Verify Copy by using after move into HashSet
    let _ = a;
}
