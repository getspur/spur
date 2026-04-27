use spur_license::policy::feature_key::FeatureKey;
use std::collections::HashSet;

#[test]
fn known_features_exist() {
    assert_eq!(
        FeatureKey::CORE_CORE_BRAIN_SESSION.as_str(),
        "core_core_brain_session"
    );
    assert_eq!(
        FeatureKey::CORE_CORE_PARALLEL_WORKERS.as_str(),
        "core_core_parallel_workers"
    );
}

#[test]
fn from_known_parses_all_keys() {
    // Spot-check a few from each tier
    assert_eq!(
        FeatureKey::from_known("core_core_brain_session"),
        Some(FeatureKey::CORE_CORE_BRAIN_SESSION)
    );
    assert_eq!(
        FeatureKey::from_known("core_pro_review_auto_approve"),
        Some(FeatureKey::CORE_PRO_REVIEW_AUTO_APPROVE)
    );
    assert_eq!(
        FeatureKey::from_known("pm_core_browse"),
        Some(FeatureKey::PM_CORE_BROWSE)
    );
    assert_eq!(
        FeatureKey::from_known("blob_pro_namespace_deletion"),
        Some(FeatureKey::BLOB_PRO_NAMESPACE_DELETION)
    );
    // Unknown strings must return None
    assert_eq!(FeatureKey::from_known("nonexistent_feature"), None);
    assert_eq!(FeatureKey::from_known("enable_browser_tool"), None);
    assert_eq!(FeatureKey::from_known(""), None);
}

#[test]
fn feature_key_is_copy_and_hashable() {
    let a = FeatureKey::CORE_CORE_BRAIN_SESSION;
    let b = FeatureKey::CORE_CORE_BRAIN_SESSION;
    assert_eq!(a, b);

    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));

    // Verify Copy by using after move into HashSet
    let _ = a;
}
