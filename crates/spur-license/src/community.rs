//! `LicenseProvider` impl for the no-LicenseSeat-config case.
//!
//! Reads the embedded signed PolicyDocument; exposes the `community` tier's
//! entitlements; never emits events; rejects `activate` (the facade or CLI
//! routes that to the LicenseSeat path under Option A).

use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::licenseseat::LicenseSeatProvider;
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
        // The embedded signed policy now defines `community`, `pro`,
        // `team`, and `enterprise` tier blocks (Team and Enterprise
        // currently mirror Pro entitlements as placeholders until
        // their feature deltas are specified — Plan E.h hygiene wave
        // for Team; future tier-design pass for Enterprise). Unknown
        // values fall back to community with a debug log.
        #[cfg(debug_assertions)]
        let (features, plan) = match std::env::var(DEV_PLAN_ENV).ok() {
            Some(ref v) if v == "pro" => (resolve_features("pro"), Plan::Pro),
            Some(ref v) if v == "team" => (resolve_features("team"), Plan::Team),
            Some(ref v) if v == "enterprise" => (resolve_features("enterprise"), Plan::Enterprise),
            Some(v) if !v.is_empty() => {
                tracing::debug!(
                    requested = %v,
                    "SPUR_LICENSE_DEV_PLAN value is not a known tier label; \
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

/// Community-tier surface that delegates `activate` to a baked-in
/// `LicenseSeatProvider`. Used in Option A release builds (per the
/// 2026-04-19 plan, Task 14b) when no cached license is present: the
/// user sees the Community state everywhere, but `spur auth login --key …`
/// works without any runtime env-var setup because the publishable
/// credentials are baked into the binary at build time.
///
/// On successful activation, the underlying LicenseSeat SDK persists
/// the license cache to disk; the NEXT process launch comes up as a
/// bare `LicenseSeatProvider` directly via the `has_cached_license()`
/// branch in `licenseseat::from_env_or_disabled`.
pub struct CommunityProviderWithUpgrade {
    community: CommunityProvider,
    upgrade_target: Arc<LicenseSeatProvider>,
}

impl CommunityProviderWithUpgrade {
    pub fn new(resolver: Arc<PolicyResolver>, upgrade_target: Arc<LicenseSeatProvider>) -> Self {
        Self {
            community: CommunityProvider::new(resolver),
            upgrade_target,
        }
    }
}

#[async_trait]
impl LicenseProvider for CommunityProviderWithUpgrade {
    fn current_state(&self) -> LicenseState {
        // Until activation succeeds + the next launch promotes us via
        // the cache, we render as Community.
        self.community.current_state()
    }
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        // Subscribe to the community provider's (silent) channel until
        // upgrade. The activation call delegates to the LicenseSeat SDK
        // which fires its own events but only after the next launch
        // promotes the provider.
        self.community.subscribe()
    }
    fn refresh_policy(&self) -> RefreshPolicy {
        self.community.refresh_policy()
    }
    fn requires_heartbeat(&self) -> bool {
        false
    }
    fn has_entitlement(&self, feature: &str) -> bool {
        self.community.has_entitlement(feature)
    }
    async fn activate(&self, key: &str) -> Result<LicenseState> {
        // Delegate to the baked-in LicenseSeatProvider. On success the
        // SDK persists a license cache to disk. The next process launch
        // picks up the cache and promotes to bare LicenseSeatProvider.
        let activated = self.upgrade_target.activate(key).await?;
        // The activation response only carries `plan_key` — its
        // `features` set is empty. Without a follow-up validate the
        // in-process FeatureGate would stay on Community entitlements
        // until the next process restart (when `hydrate_from_cached`
        // reads `active_entitlements` off disk). Best-effort validate
        // closes that gap. If it fails (e.g. transient network), the
        // activation already succeeded server-side and the cache is on
        // disk, so the next launch hydrates fully — return the
        // partial-but-active activation state.
        match self.upgrade_target.validate().await {
            Ok(validated) => Ok(validated),
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "post-activate validate failed; entitlements will load on next launch"
                );
                Ok(activated)
            }
        }
    }
    async fn validate(&self) -> Result<LicenseState> {
        self.community.validate().await
    }
    async fn heartbeat(&self) -> Result<LicenseState> {
        self.community.heartbeat().await
    }
    async fn deactivate(&self) -> Result<LicenseState> {
        self.community.deactivate().await
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
