use std::sync::Arc;

use spur_cli::commands::auth::{run_with_license, AuthCommands, OutputFormat};
use spur_license::policy::PolicyResolver;
use spur_license::test_support::FakeProvider;
use spur_license::{EntitlementSnapshot, FeatureGate, FeatureKey, LicenseState, Plan, SpurLicense};

fn test_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

#[tokio::test]
async fn login_happy_path_activates_and_prints() {
    let fake = Arc::new(FakeProvider::new(LicenseState::inactive("fresh")));
    fake.push_activate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());

    run_with_license(
        AuthCommands::Login {
            key: "test-key".into(),
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect("login happy path");
    assert_eq!(
        fake.activate_call_count(),
        1,
        "activate should be called exactly once"
    );
    // validate is not invoked during Login.
    assert_eq!(fake.validate_call_count(), 0);
}

#[tokio::test]
async fn refresh_invokes_validate_exactly_once() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    fake.push_validate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());

    run_with_license(
        AuthCommands::Refresh {
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect("refresh happy path");
    assert_eq!(fake.validate_call_count(), 1);
}

#[tokio::test]
async fn logout_invokes_deactivate_and_transitions_to_inactive() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());

    run_with_license(
        AuthCommands::Logout {
            format: OutputFormat::Plain,
        },
        license.clone(),
    )
    .await
    .expect("logout happy path");
    assert_eq!(fake.deactivate_call_count(), 1);
    assert!(matches!(
        license.current_state().status,
        spur_license::LicenseStatus::Inactive
    ));
}

#[tokio::test]
async fn login_fails_on_config_error_state() {
    // When the facade's current_state() reports ConfigError (as DisabledProvider
    // does), the CLI must refuse to activate even if a provider is technically
    // available.
    use spur_license::LicenseStatus;
    let mut seed = LicenseState::inactive("unconfigured");
    seed.status = LicenseStatus::ConfigError;
    let fake = Arc::new(FakeProvider::new(seed));
    let license = SpurLicense::from_provider(fake, test_feature_gate());

    let res = run_with_license(
        AuthCommands::Login {
            key: "nope".into(),
            format: OutputFormat::Plain,
        },
        license,
    )
    .await;
    assert!(res.is_err(), "login must error on ConfigError seed");
}

// ---------- Plan C M0.5 — `cli_core_license_activate` enforcement ----------
//
// Login is gated INSIDE `auth::run` on the `Login` variant only.
// Logout / Refresh / Status remain ungated so a tampered tier never
// bricks the recovery path: the user can always run `spur auth logout`
// to fall back to the embedded community policy where the key IS
// granted, and re-login succeeds from there.

/// Build a `FeatureGate` whose snapshot has zero entitlements. Pro/Team/
/// Enterprise tiers inherit the Community baseline from the signed policy
/// (`@inherit:community`), so an empty JWT alone is no longer enough to
/// strip a key — we have to inject a hand-crafted empty snapshot. The
/// binary-level analog is `SPUR_LICENSE_TEST_STRIP_KEYS` exercised by
/// `cli_core_gate_e2e::spur_auth_login_exits_nonzero_*`.
fn stripped_gate() -> Arc<FeatureGate> {
    let g = FeatureGate::new(PolicyResolver::embedded());
    g.set_snapshot_for_test(EntitlementSnapshot::default());
    Arc::new(g)
}

#[tokio::test]
async fn login_blocked_by_stripped_gate_returns_typed_error() {
    let fake = Arc::new(FakeProvider::new(LicenseState::inactive("fresh")));
    fake.push_activate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone(), stripped_gate());

    let err = run_with_license(
        AuthCommands::Login {
            key: "test-key".into(),
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect_err("login must be gated when cli_core_license_activate is absent");

    let msg = format!("{err:#}");
    assert!(
        msg.contains(FeatureKey::CLI_CORE_LICENSE_ACTIVATE.as_str()),
        "error must name the gated key: {msg}"
    );
    assert_eq!(
        fake.activate_call_count(),
        0,
        "gate must fire BEFORE provider.activate to prevent escalation against a tampered tier",
    );
}

#[tokio::test]
async fn logout_passes_through_stripped_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), stripped_gate());
    run_with_license(
        AuthCommands::Logout {
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect("logout must remain ungated to preserve brick recovery path");
    assert_eq!(fake.deactivate_call_count(), 1);
}

#[tokio::test]
async fn refresh_passes_through_stripped_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    fake.push_validate_result(Ok(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), stripped_gate());
    run_with_license(
        AuthCommands::Refresh {
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect("refresh must remain ungated to preserve brick recovery path");
    assert_eq!(fake.validate_call_count(), 1);
}

#[tokio::test]
async fn status_passes_through_stripped_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake, stripped_gate());
    run_with_license(
        AuthCommands::Status {
            format: OutputFormat::Plain,
        },
        license,
    )
    .await
    .expect("status must remain ungated to preserve brick recovery path");
    // Status doesn't invoke any provider RPC; just reads current_state.
}
