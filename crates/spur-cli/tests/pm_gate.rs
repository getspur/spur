use spur_license::policy::PolicyResolver;
use spur_license::{EntitlementSnapshot, FeatureGate, FeatureKey};

#[test]
fn pm_service_construction_gate_allows_embedded_free_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert!(gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(spur_cli::pm_service_gate_allows_construction(&gate));
}

#[test]
fn pm_service_construction_gate_blocks_state_without_pm_browse() {
    // Pro tier inherits Community via the policy's `@inherit:community`
    // directive, so an empty JWT does NOT actually strip pm_core_browse —
    // the only way to simulate a snapshot missing this key is the
    // test-support hook (binary-level analog: SPUR_LICENSE_TEST_STRIP_KEYS).
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    gate.set_snapshot_for_test(EntitlementSnapshot::default());

    assert!(!gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(!spur_cli::pm_service_gate_allows_construction(&gate));
}
