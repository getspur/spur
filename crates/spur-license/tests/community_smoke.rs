use spur_license::{FeatureKey, QuotaKey, QuotaValue, SpurLicense, Tier};

#[test]
fn community_default_has_expected_features() {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();

    assert_eq!(gate.tier(), Tier::Community);
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(gate.has(FeatureKey::WORKTREE_ISOLATION));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
    assert!(!gate.has(FeatureKey::PM_INTEGRATION));
}

#[test]
fn community_default_quotas() {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(128 * 1024 * 1024))
    );
}
