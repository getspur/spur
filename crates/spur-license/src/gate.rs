use std::collections::HashMap;
use std::sync::Arc;

use ahash::AHashSet;
use arc_swap::ArcSwap;

use crate::policy::{FeatureKey, FlagSpec, PolicyDocument, PolicyResolver};
use crate::quota::{QuotaKey, QuotaValue};
use crate::snapshot::{EntitlementSnapshot, SourceMetadata};
use crate::tier::Tier;
use crate::{LicenseState, Plan};

pub struct FeatureGate {
    snapshot: ArcSwap<EntitlementSnapshot>,
    policy: Arc<PolicyResolver>,
}

impl FeatureGate {
    pub fn new(policy: Arc<PolicyResolver>) -> Self {
        let snapshot = Self::build_community_snapshot(&policy);
        Self {
            snapshot: ArcSwap::new(Arc::new(snapshot)),
            policy,
        }
    }

    /// Wait-free feature check.
    pub fn has(&self, feature: FeatureKey) -> bool {
        self.snapshot.load().features.contains(&feature)
    }

    /// Wait-free quota read.
    pub fn quota(&self, key: QuotaKey) -> Option<QuotaValue> {
        self.snapshot.load().quotas.get(&key).copied()
    }

    pub fn tier(&self) -> Tier {
        self.snapshot.load().tier
    }

    pub fn snapshot(&self) -> arc_swap::Guard<Arc<EntitlementSnapshot>> {
        self.snapshot.load()
    }

    pub fn update_state(&self, state: &LicenseState) {
        let new_snapshot = self.build_snapshot(state);
        self.snapshot.store(Arc::new(new_snapshot));
    }

    fn build_community_snapshot(policy: &PolicyResolver) -> EntitlementSnapshot {
        let features: AHashSet<FeatureKey> = policy
            .tier_features("community")
            .into_iter()
            .filter_map(|s| FeatureKey::from_known(&s))
            .collect();

        let quotas = Self::merge_quotas(Tier::Community);
        let flags = Self::extract_flags(policy.document());

        EntitlementSnapshot {
            tier: Tier::Community,
            features,
            quotas,
            flags,
            source: SourceMetadata {
                plan: Plan::Community,
                expires_at: None,
                is_offline: true,
            },
        }
    }

    fn build_snapshot(&self, state: &LicenseState) -> EntitlementSnapshot {
        if !state.is_active() {
            return EntitlementSnapshot::default();
        }

        let tier = Tier::from_plan(state.plan);
        let features: AHashSet<FeatureKey> = if tier == Tier::Community {
            self.policy
                .tier_features("community")
                .into_iter()
                .filter_map(|s| FeatureKey::from_known(&s))
                .collect()
        } else {
            state
                .features
                .iter()
                .filter_map(|s| FeatureKey::from_known(s))
                .collect()
        };

        let quotas = Self::merge_quotas(tier);
        let flags = Self::extract_flags(self.policy.document());

        EntitlementSnapshot {
            tier,
            features,
            quotas,
            flags,
            source: SourceMetadata {
                plan: state.plan,
                expires_at: state.expires_at,
                is_offline: state.offline_ok,
            },
        }
    }

    fn merge_quotas(tier: Tier) -> HashMap<QuotaKey, QuotaValue> {
        let mut quotas = HashMap::new();
        match tier {
            Tier::Community => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(1));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(128 * 1024 * 1024),
                );
            }
            Tier::Pro => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(5));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(1024 * 1024 * 1024),
                );
            }
            Tier::Team => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(10));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(10 * 1024 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::MinSeats, QuotaValue::Count(3));
            }
            Tier::Enterprise => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Unlimited);
            }
        }
        quotas
    }

    fn extract_flags(doc: Arc<PolicyDocument>) -> HashMap<FeatureKey, FlagSpec> {
        doc.flags
            .iter()
            .filter_map(|(k, v)| FeatureKey::from_known(k).map(|key| (key, v.clone())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn inactive_license_is_fail_closed() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);
        let inactive = LicenseState::inactive("test inactive");
        gate.update_state(&inactive);
        assert!(!gate.has(FeatureKey::BRAIN_SESSION));
        assert_eq!(gate.quota(QuotaKey::MaxConcurrentWorkers), None);
    }

    #[test]
    fn tier_transition_updates_atomically() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);

        // Start as community
        assert_eq!(gate.tier(), Tier::Community);
        assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));

        // Update to Pro with parallel_workers
        let mut features = BTreeSet::new();
        features.insert("parallel_workers".to_string());
        let pro_state = LicenseState::active_validated(Plan::Pro, features);
        gate.update_state(&pro_state);

        assert_eq!(gate.tier(), Tier::Pro);
        assert!(gate.has(FeatureKey::PARALLEL_WORKERS));
    }
}
