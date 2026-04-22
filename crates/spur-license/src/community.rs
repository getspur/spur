//! `LicenseProvider` impl for the no-LicenseSeat-config case.
//!
//! Reads the embedded signed PolicyDocument; exposes the `community` tier's
//! entitlements; never emits events; rejects `activate` (the facade or CLI
//! routes that to the LicenseSeat path under Option A).

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::policy::PolicyResolver;
use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{LicenseError, LicenseEvent, LicenseState, Plan, Result};

const DEV_PLAN_ENV: &str = "SPUR_LICENSE_DEV_PLAN";

pub struct CommunityProvider {
    state: LicenseState,
    /// Constructed but never sent on. Exists to satisfy the trait's
    /// `subscribe()`. Single-emission-seam invariant preserved.
    events_tx: broadcast::Sender<LicenseEvent>,
}

impl CommunityProvider {
    pub fn new(resolver: Arc<PolicyResolver>) -> Self {
        // Dev-tier override is compile-gated to debug builds only.
        // It CANNOT leak into release/production binaries.
        #[cfg(debug_assertions)]
        let (features, plan) = match std::env::var(DEV_PLAN_ENV).ok().as_deref() {
            Some("pro") => (resolver.tier_features("pro"), Plan::Pro),
            Some("team") => (resolver.tier_features("team"), Plan::Team),
            Some("enterprise") => (resolver.tier_features("enterprise"), Plan::Enterprise),
            _ => (resolver.tier_features("community"), Plan::Community),
        };
        #[cfg(not(debug_assertions))]
        let (features, plan) = (resolver.tier_features("community"), Plan::Community);

        let state = if plan == Plan::Community {
            LicenseState::active_community(features)
        } else {
            LicenseState::active_validated(plan, features)
        };

        let (events_tx, _) = broadcast::channel(1);
        Self { state, events_tx }
    }
}

#[async_trait]
impl LicenseProvider for CommunityProvider {
    fn current_state(&self) -> LicenseState {
        self.state.clone()
    }
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.events_tx.subscribe()
    }
    fn refresh_policy(&self) -> RefreshPolicy {
        RefreshPolicy::default()
    }
    fn requires_heartbeat(&self) -> bool {
        false
    }
    fn has_entitlement(&self, feature: &str) -> bool {
        self.state.features.contains(feature)
    }
    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        Err(LicenseError::NotConfigured(
            "Community tier provider cannot activate license keys directly. \
             Build with SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY/PRODUCT_SLUG, \
             or set the matching SPUR_LICENSESEAT_* runtime env vars to upgrade."
                .into(),
        ))
    }
    async fn validate(&self) -> Result<LicenseState> {
        Ok(self.state.clone())
    }
    async fn heartbeat(&self) -> Result<LicenseState> {
        Ok(self.state.clone())
    }
    async fn deactivate(&self) -> Result<LicenseState> {
        Ok(self.state.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::policy::PolicyResolver;

    // Guard env-var mutations because rust test runner is multi-threaded.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn community_provider_reports_community_features() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Ensure no dev override is present.
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, crate::Plan::Community));
        assert!(p.has_entitlement("brain_session"));
        assert!(p.has_entitlement("single_worker"));
        assert!(!p.has_entitlement("parallel_workers"));
    }

    #[tokio::test]
    async fn community_provider_rejects_activate() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let err = p.activate("any-key").await.unwrap_err();
        assert!(matches!(err, LicenseError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn community_provider_validate_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let s1 = p.validate().await.unwrap();
        let s2 = p.validate().await.unwrap();
        assert_eq!(s1.features, s2.features);
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    async fn dev_plan_override_reports_pro_features() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(DEV_PLAN_ENV, "pro");
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, Plan::Pro));
        assert!(p.has_entitlement("parallel_workers"));
        assert!(p.has_entitlement("auto_review_policies"));
        std::env::remove_var(DEV_PLAN_ENV);
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    async fn dev_plan_override_unrecognized_defaults_to_community() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(DEV_PLAN_ENV, "mystery");
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, Plan::Community));
        std::env::remove_var(DEV_PLAN_ENV);
    }
}
