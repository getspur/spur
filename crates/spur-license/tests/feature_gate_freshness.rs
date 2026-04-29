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
    license
        .activate("PRO_KEY")
        .await
        .expect("activate should succeed");

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
    license
        .deactivate()
        .await
        .expect("deactivate should succeed");

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

#[tokio::test]
async fn validate_err_keeps_gate_unchanged() {
    use spur_license::LicenseError;

    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    // Clone the Arc so we hold a strong reference to the OLD allocation
    // across the mutation. Arc::ptr_eq compares by allocation identity,
    // catching any spurious update_state call (value equality would
    // succeed even on a no-op refresh).
    let pre_arc = Arc::clone(&*cached.snapshot());

    fake.push_validate_result(Err(LicenseError::Provider(
        "transient network failure".into(),
    )));
    let result = license.validate().await;

    assert!(result.is_err(), "validate must propagate the provider Err");
    let post_arc = Arc::clone(&*cached.snapshot());
    assert!(
        Arc::ptr_eq(&pre_arc, &post_arc),
        "non-mutating validate-Err must NOT trigger update_state \
         (Arc::ptr_eq proves no store happened)",
    );
}

#[tokio::test]
async fn heartbeat_err_with_degrade_refreshes_cached_gate_to_provider_state() {
    use spur_license::{LicenseError, LicenseStatus};

    let (fake, license, _g) = build_license_with_community_seed();
    // Activate Pro first so heartbeat has degraded-Pro state to commit.
    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.unwrap();
    let cached = license.feature_gate();
    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
    // Clone the OLD Arc so we hold a strong reference across the
    // mutation; Arc::ptr_eq below proves a store happened.
    let pre_arc = Arc::clone(&*cached.snapshot());

    // Build a degraded-Pro state: Pro plan with degraded status.
    let mut degraded_pro = pro_state();
    degraded_pro.status = LicenseStatus::Degraded;
    degraded_pro.status_text = "heartbeat failed".into();

    fake.push_heartbeat_degraded_err(
        degraded_pro.clone(),
        LicenseError::Provider("heartbeat failed: simulated".into()),
    );
    let result = license.heartbeat().await;
    assert!(result.is_err(), "heartbeat must propagate the provider Err");

    // 1. A store happened (Arc allocation identity changed).
    let post_arc = Arc::clone(&*cached.snapshot());
    assert!(
        !Arc::ptr_eq(&pre_arc, &post_arc),
        "heartbeat-Err must trigger update_state on the cached gate",
    );

    // 2. The new snapshot was sourced from provider.current_state(),
    //    which after FakeProvider's commit holds `degraded_pro`. Prove
    //    this by checking the snapshot's source plan is Pro (the
    //    degraded state's plan), NOT a synthesized fallback.
    assert_eq!(
        post_arc.source.plan,
        Plan::Pro,
        "gate's snapshot.source.plan must reflect the degraded-Pro state \
         (proves refresh source was provider.current_state(), not a hardcoded fallback)",
    );

    // 3. Sanity check: license.current_state() also reports degraded.
    let live = license.current_state();
    assert!(
        matches!(live.status, LicenseStatus::Degraded),
        "license.current_state() must report Degraded after heartbeat-Err-with-degrade",
    );
}
