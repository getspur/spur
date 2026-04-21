#![cfg(feature = "test-support")]

use std::sync::Arc;

use proptest::prelude::*;
use spur_license::test_support::FakeProvider;
use spur_license::{LicenseError, LicenseState, LicenseStatus, SpurLicense};

#[derive(Debug, Clone, Copy)]
enum Step {
    ValidateNetworkErr,
    HeartbeatNetworkErr,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        Just(Step::ValidateNetworkErr),
        Just(Step::HeartbeatNetworkErr),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Trust invariant: no sequence of network-only validate/heartbeat failures
    /// may ever transition an Active license to Invalid. Only an authoritative
    /// `valid=false` response from the provider is allowed to do that.
    #[test]
    fn network_errors_never_produce_invalid_from_active(
        script in prop::collection::vec(step_strategy(), 1..32),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async move {
            let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
            let policy = spur_license::policy::PolicyResolver::with_default_overlay();
            let feature_gate = Arc::new(spur_license::FeatureGate::new(policy));
            let license = SpurLicense::from_provider(fake.clone(), feature_gate);

            for step in &script {
                match step {
                    Step::ValidateNetworkErr => {
                        fake.push_validate_result(Err(LicenseError::Provider(
                            "network".into(),
                        )));
                        let _ = license.validate().await;
                    }
                    Step::HeartbeatNetworkErr => {
                        fake.push_heartbeat_result(Err(LicenseError::Provider(
                            "network".into(),
                        )));
                        let _ = license.heartbeat().await;
                    }
                }
            }

            prop_assert!(
                !matches!(license.current_state().status, LicenseStatus::Invalid),
                "network-only failures must not mark license Invalid"
            );
            Ok(())
        }).unwrap();
    }
}
