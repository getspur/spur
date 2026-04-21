use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, QuotaKey, QuotaValue, Tier};

#[test]
fn community_has_core_features() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
    assert_eq!(gate.tier(), Tier::Community);
}

#[test]
fn community_quota_defaults() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
}

#[test]
fn unknown_feature_returns_false() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
}
