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
use crate::{LicenseError, LicenseEvent, LicenseState, Result};

pub struct CommunityProvider {
    state: LicenseState,
    /// Constructed but never sent on. Exists to satisfy the trait's
    /// `subscribe()`. Single-emission-seam invariant preserved.
    events_tx: broadcast::Sender<LicenseEvent>,
}

impl CommunityProvider {
    pub fn new(resolver: Arc<PolicyResolver>) -> Self {
        let features = resolver.tier_features("community");
        let state = LicenseState::active_community(features);
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
    use super::*;
    use crate::policy::PolicyResolver;

    #[tokio::test]
    async fn community_provider_reports_community_features() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, crate::Plan::Community));
        assert!(p.has_entitlement("brain_session"));
        assert!(p.has_entitlement("single_worker"));
        assert!(!p.has_entitlement("parallel_workers"));
    }

    #[tokio::test]
    async fn community_provider_rejects_activate() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let err = p.activate("any-key").await.unwrap_err();
        assert!(matches!(err, LicenseError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn community_provider_validate_is_idempotent() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let s1 = p.validate().await.unwrap();
        let s2 = p.validate().await.unwrap();
        assert_eq!(s1.features, s2.features);
    }
}
