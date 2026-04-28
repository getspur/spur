//! `LicenseProvider` impl for the no-LicenseSeat-config case.
//!
//! Reads the embedded signed PolicyDocument; exposes the `community` tier's
//! entitlements; never emits events; rejects `activate` (the facade or CLI
//! routes that to the LicenseSeat path under Option A).

use async_trait::async_trait;
use std::collections::BTreeSet;
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
        let resolve_features = |tier: &str| -> BTreeSet<String> {
            resolver.tier_features(tier).unwrap_or_else(|err| {
                tracing::warn!("policy tier {tier:?} malformed: {err}; using empty features");
                BTreeSet::new()
            })
        };

        // Dev-tier override is compile-gated to debug builds only.
        // It CANNOT leak into release/production binaries.
        //
        // Only `pro` is honored: the embedded signed policy at
        // `crates/spur-license/resources/default_policy.json` defines
        // tier blocks for `community` and `pro` only (see
        // `2026-04-28-tier-revamp-policy-gap-enterprise-tier.md`). When
        // `team`/`enterprise` were honored as plan labels with empty
        // resolved features, every gate denied — masking real bugs.
        // Any unknown value (including `team`/`enterprise`) falls back
        // to community with a debug log naming the attempted value.
        #[cfg(debug_assertions)]
        let (features, plan) = match std::env::var(DEV_PLAN_ENV).ok() {
            Some(ref v) if v == "pro" => (resolve_features("pro"), Plan::Pro),
            Some(v) if !v.is_empty() => {
                tracing::debug!(
                    requested = %v,
                    "SPUR_LICENSE_DEV_PLAN value is not embedded in default_policy.json; \
                     falling back to community"
                );
                (resolve_features("community"), Plan::Community)
            }
            _ => (resolve_features("community"), Plan::Community),
        };
        #[cfg(not(debug_assertions))]
        let (features, plan) = (resolve_features("community"), Plan::Community);

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
    use tokio::sync::Mutex;

    use super::*;
    use crate::policy::PolicyResolver;
    use crate::FeatureGate;
    use crate::{QuotaKey, QuotaValue};

    // Guard env-var mutations because rust test runner is multi-threaded.
    // Use `tokio::sync::Mutex` (not `std::sync::Mutex`) so the guard can be
    // held across `.await` points without tripping clippy::await_holding_lock
    // (and without risking executor blocking).
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    #[tokio::test]
    async fn community_provider_reports_community_features() {
        let _guard = ENV_LOCK.lock().await;
        // Ensure no dev override is present.
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, crate::Plan::Community));
        assert!(p.has_entitlement("core_core_brain_session"));
        assert!(p.has_entitlement("core_core_parallel_workers"));
        let gate = FeatureGate::new(PolicyResolver::embedded());
        gate.update_state(&state);
        assert_eq!(
            gate.quota(QuotaKey::MaxConcurrentWorkers),
            Some(QuotaValue::Count(1))
        );
        assert!(!p.has_entitlement("pm_pro_beads_advanced"));
    }

    #[tokio::test]
    async fn community_provider_rejects_activate() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let err = p.activate("any-key").await.unwrap_err();
        assert!(matches!(err, LicenseError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn community_provider_validate_is_idempotent() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(DEV_PLAN_ENV);
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let s1 = p.validate().await.unwrap();
        let s2 = p.validate().await.unwrap();
        assert_eq!(s1.features, s2.features);
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    async fn dev_plan_override_reports_pro_features() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var(DEV_PLAN_ENV, "pro");
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, Plan::Pro));
        assert!(p.has_entitlement("core_core_parallel_workers"));
        assert!(p.has_entitlement("core_pro_review_auto_approve"));
        std::env::remove_var(DEV_PLAN_ENV);
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    async fn dev_plan_override_unrecognized_defaults_to_community() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var(DEV_PLAN_ENV, "mystery");
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, Plan::Community));
        std::env::remove_var(DEV_PLAN_ENV);
    }
}
