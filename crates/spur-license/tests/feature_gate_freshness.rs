//! Regression tests for bd-22q.1: SpurLicense::feature_gate() must
//! return a fresh Arc<FeatureGate> after every successful mutating
//! call. The tests capture the cached Arc once and assert its
//! contents change in place — proving propagation through the
//! shared allocation, which is what caching consumers (Orchestrator,
//! CLI subcommands, MCP) rely on.
//!
//! Spec: docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md

use std::collections::BTreeSet;
use std::sync::Arc;

use spur_license::policy::FeatureKey;
use spur_license::test_support::FakeProvider;
use spur_license::{FeatureGate, LicenseState, Plan, SpurLicense};

fn build_license_with_community_seed() -> (Arc<FakeProvider>, SpurLicense, Arc<FeatureGate>) {
    let mut features = BTreeSet::new();
    features.insert("chat".to_string());
    let community = LicenseState::active_community(features);
    let fake = Arc::new(FakeProvider::new(community.clone()));
    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let gate = Arc::new(FeatureGate::new(policy));
    gate.update_state(&community);
    let license = SpurLicense::from_provider(fake.clone(), gate.clone());
    (fake, license, gate)
}

fn pro_state() -> LicenseState {
    let mut feats = BTreeSet::new();
    // Real Pro-only feature key per spec; not in Community policy overlay.
    feats.insert("blob_pro_namespace_deletion".to_string());
    LicenseState::active_validated(Plan::Pro, feats)
}

#[tokio::test]
async fn validate_pro_state_refreshes_cached_gate() {
    let (fake, license, _gate_for_lifetime) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(
        !cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "Community baseline must not have Pro entitlement",
    );

    fake.push_validate_result(Ok(pro_state()));
    license.validate().await.expect("validate should succeed");

    assert!(
        cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "cached Arc<FeatureGate> must reflect Pro after validate",
    );
}

#[tokio::test]
async fn activate_pro_state_refreshes_cached_gate() {
    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(!cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.expect("activate should succeed");

    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
}

#[tokio::test]
async fn deactivate_refreshes_cached_gate_to_inactive() {
    let (fake, license, _g) = build_license_with_community_seed();
    // First activate to Pro so deactivate has something to clear.
    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.unwrap();
    let cached = license.feature_gate();
    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_deactivate_result(Ok(LicenseState::inactive("user requested")));
    license.deactivate().await.expect("deactivate should succeed");

    assert!(
        !cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "deactivate must drop Pro entitlement from cached gate",
    );
}

#[tokio::test]
async fn heartbeat_ok_refreshes_cached_gate() {
    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(!cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_heartbeat_result(Ok(pro_state()));
    license.heartbeat().await.expect("heartbeat should succeed");

    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
}
