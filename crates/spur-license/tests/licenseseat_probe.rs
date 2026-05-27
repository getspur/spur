#![allow(unsafe_code)] // std::env::set_var is unsafe in Rust 2024; test-only setup.

use std::sync::Mutex;

use spur_license::{LicenseState, LicenseStatus, Plan, SpurLicense};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn disabled_env_path_returns_config_error_or_inactive_state() {
    let _guard = ENV_LOCK.lock().unwrap();
    let state = SpurLicense::from_env_or_disabled().current_state();
    assert!(matches!(
        state.status,
        LicenseStatus::ConfigError | LicenseStatus::Inactive | LicenseStatus::Active
    ));
}

#[test]
fn configured_env_path_does_not_require_a_tokio_runtime() {
    let _guard = ENV_LOCK.lock().unwrap();
    let prev_api_key = std::env::var_os("SPUR_LICENSESEAT_API_KEY");
    let prev_product_slug = std::env::var_os("SPUR_LICENSESEAT_PRODUCT_SLUG");

    unsafe {
        std::env::set_var("SPUR_LICENSESEAT_API_KEY", "test-api-key");
        std::env::set_var("SPUR_LICENSESEAT_PRODUCT_SLUG", "test-product");
    }

    let result = std::panic::catch_unwind(|| SpurLicense::from_env_or_disabled().current_state());

    unsafe {
        match prev_api_key {
            Some(value) => std::env::set_var("SPUR_LICENSESEAT_API_KEY", value),
            None => std::env::remove_var("SPUR_LICENSESEAT_API_KEY"),
        }
        match prev_product_slug {
            Some(value) => std::env::set_var("SPUR_LICENSESEAT_PRODUCT_SLUG", value),
            None => std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG"),
        }
    }

    let state = result.expect("configured license path should not panic without Tokio");
    assert!(matches!(
        state.status,
        LicenseStatus::Inactive | LicenseStatus::Active
    ));
}

#[test]
fn plan_labels_are_human_readable() {
    assert_eq!(Plan::Pro.label(), "Pro");
    assert_eq!(Plan::Unknown.label(), "Licensed");
}

#[test]
fn degraded_state_is_active_for_gating_purposes() {
    let mut state = LicenseState::active_cached();
    state.status = LicenseStatus::Degraded;
    assert!(state.is_active());
}

#[tokio::test]
#[ignore = "requires live LicenseSeat credentials"]
async fn live_validate_smoke() {
    let license = SpurLicense::from_env().expect("license env configured");
    let _ = license.validate().await.expect("validate");
}

#[test]
#[ignore = "requires a cached live activation"]
fn cached_active_startup_surfaces_real_plan() {
    // Precondition: an earlier run called activate() with a non-community
    // key so `trusted_license` is present in the disk cache.
    let license = SpurLicense::from_env().expect("env configured");
    let state = license.current_state();
    assert!(matches!(state.status, spur_license::LicenseStatus::Active));
    assert!(
        !matches!(state.plan, spur_license::Plan::Unknown),
        "expected cached plan to hydrate, got Unknown (plan={:?})",
        state.plan,
    );
}

#[test]
fn from_provider_returns_a_usable_facade() {
    use spur_license::provider::LicenseProvider;
    use std::sync::Arc;

    // Inline minimal provider so this test doesn't depend on FakeProvider
    // (which lands in Task 4).
    struct Noop;
    #[async_trait::async_trait]
    impl LicenseProvider for Noop {
        fn current_state(&self) -> spur_license::LicenseState {
            spur_license::LicenseState::inactive("noop")
        }
        fn subscribe(&self) -> tokio::sync::broadcast::Receiver<spur_license::LicenseEvent> {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            std::mem::forget(tx);
            rx
        }
        fn refresh_policy(&self) -> spur_license::provider::RefreshPolicy {
            spur_license::provider::RefreshPolicy::default()
        }
        fn has_entitlement(&self, _: &str) -> bool {
            false
        }
        async fn activate(&self, _: &str) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn validate(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn heartbeat(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn deactivate(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
    }

    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let feature_gate = Arc::new(spur_license::FeatureGate::new(policy));
    let license = SpurLicense::from_provider(Arc::new(Noop), feature_gate);
    assert!(matches!(
        license.current_state().status,
        spur_license::LicenseStatus::Inactive
    ));
}
