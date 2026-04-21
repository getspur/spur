use std::sync::Arc;

use spur_cli::commands::auth::{run_with_license, AuthCommands, OutputFormat};
use spur_license::policy::PolicyResolver;
use spur_license::test_support::FakeProvider;
use spur_license::FeatureGate;
use spur_license::{LicenseState, Plan, SpurLicense};

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
