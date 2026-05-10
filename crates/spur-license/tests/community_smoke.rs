//! Smoke tests for the embedded Community-tier defaults.
//!
//! These tests construct `CommunityProvider` directly to bypass environment
//! variable overrides (`SPUR_LICENSE_DEV_PLAN` from the dev-overlay path,
//! `LICENSESEAT_*` from the licenseseat-provider path). `from_env_or_disabled`
//! is environment-dependent by design; these tests verify the *default*
//! community policy regardless of the developer's env. We also explicitly
//! clear the dev-overlay env var (debug builds respect it) before each test.

use spur_license::policy::PolicyResolver;
use spur_license::{
    CommunityProvider, FeatureGate, FeatureKey, QuotaKey, QuotaValue, SpurLicense, Tier,
};
use std::sync::Arc;

const DEV_PLAN_ENV: &str = "SPUR_LICENSE_DEV_PLAN";

fn community_facade() -> SpurLicense {
    // SAFETY/INTEGRATION-TEST NOTE: Tests in this file mutate process-wide env
    // vars. Cargo runs integration tests serially within one binary file by
    // default for binary linking, but separate test files run in parallel
    // processes. Mutating DEV_PLAN_ENV here is safe because no other
    // community_smoke test sets it back, and inter-file isolation is by
    // separate processes.
    std::env::remove_var(DEV_PLAN_ENV);
    let provider: Arc<dyn spur_license::LicenseProvider> =
        Arc::new(CommunityProvider::new(PolicyResolver::embedded()));
    let policy = PolicyResolver::embedded();
    let feature_gate = Arc::new(FeatureGate::new(policy));
    feature_gate.update_state(&provider.current_state());
    SpurLicense::from_provider(provider, feature_gate)
}

#[test]
fn community_default_has_expected_features() {
    let license = community_facade();
    let gate = license.feature_gate();

    assert_eq!(gate.tier(), Tier::Community);
    assert!(gate.has(FeatureKey::CORE_CORE_BRAIN_SESSION));
    assert!(gate.has(FeatureKey::CORE_CORE_PARALLEL_WORKERS));
    assert!(gate.has(FeatureKey::WORKTREE_CORE_ISOLATION));
    assert!(gate.has(FeatureKey::PM_CORE_BROWSE));
    assert!(gate.has(FeatureKey::PM_PRO_BEADS_ADVANCED));
}

#[test]
fn community_default_quotas() {
    let license = community_facade();
    let gate = license.feature_gate();

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(128 * 1024 * 1024))
    );
}
