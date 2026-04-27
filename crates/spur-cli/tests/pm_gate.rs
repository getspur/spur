use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey};
use std::collections::BTreeSet;

#[test]
fn pm_service_construction_gate_allows_embedded_free_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert!(gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(spur_cli::pm_service_gate_allows_construction(&gate));
}

#[test]
fn pm_service_construction_gate_blocks_state_without_pm_browse() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    let state =
        spur_license::LicenseState::active_validated(spur_license::Plan::Pro, BTreeSet::new());
    gate.update_state(&state);

    assert!(!gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(!spur_cli::pm_service_gate_allows_construction(&gate));
}
