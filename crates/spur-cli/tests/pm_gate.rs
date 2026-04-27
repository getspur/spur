use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey};

#[test]
fn pm_service_construction_gate_allows_embedded_free_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert!(gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(spur_cli::pm_service_gate_allows_construction(&gate));
}

#[test]
fn pm_service_construction_gate_blocks_nonexistent_tier() {
    let policy = PolicyResolver::embedded();

    assert!(!policy.tier_has_feature("nonexistent", FeatureKey::PM_CORE_BROWSE.as_str()));
}
