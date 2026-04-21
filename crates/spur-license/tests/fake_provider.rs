#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use spur_license::test_support::FakeProvider;
use spur_license::{LicenseState, LicenseStatus, Plan, SpurLicense};

#[tokio::test]
async fn fake_provider_scripted_validate_transitions() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let feature_gate = Arc::new(spur_license::FeatureGate::new(policy));
    let license = SpurLicense::from_provider(fake.clone(), feature_gate);
    let mut rx = license.subscribe();

    // Script a validate that downgrades to Invalid.
    fake.push_validate_result(Ok({
        let mut s = LicenseState::active_validated(Plan::Pro, Default::default());
        s.status = LicenseStatus::Invalid;
        s.status_text = "revoked".into();
        s
    }));

    let out = license.validate().await.expect("validate ok-with-invalid");
    assert!(matches!(out.status, LicenseStatus::Invalid));

    let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .expect("event within 200ms")
        .expect("broadcast ok");
    assert!(matches!(ev.state.status, LicenseStatus::Invalid));
}

#[tokio::test]
async fn fake_provider_simulated_network_error_preserves_active_state() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let feature_gate = Arc::new(spur_license::FeatureGate::new(policy));
    let license = SpurLicense::from_provider(fake.clone(), feature_gate);

    fake.push_validate_result(Err(spur_license::LicenseError::Provider(
        "network unreachable".into(),
    )));

    let res = license.validate().await;
    assert!(res.is_err());
    // Cached state must remain Active.
    assert!(matches!(
        license.current_state().status,
        LicenseStatus::Active
    ));
}

#[tokio::test]
async fn initial_active_state_carries_non_unknown_plan_when_seeded() {
    let mut seed = LicenseState::active_validated(Plan::Pro, Default::default());
    seed.status_text = "Cached Pro".into();
    let fake = Arc::new(FakeProvider::new(seed));
    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let feature_gate = Arc::new(spur_license::FeatureGate::new(policy));
    let license = SpurLicense::from_provider(fake, feature_gate);
    assert!(matches!(license.current_state().plan, Plan::Pro));
}
