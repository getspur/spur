use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use ahash::AHashSet;
use arc_swap::ArcSwap;

use crate::install_id::InstallId;
use crate::policy::flags::FlagEvaluator;
use crate::policy::{FeatureKey, FlagKey, FlagSpec, PolicyDocument, PolicyResolver};
use crate::quota::{QuotaKey, QuotaValue};
use crate::snapshot::{EntitlementSnapshot, SourceMetadata};
use crate::tier::Tier;
use crate::{LicenseState, Plan};

#[allow(dead_code)]
pub struct FeatureGate {
    snapshot: ArcSwap<EntitlementSnapshot>,
    policy: Arc<PolicyResolver>,
    install_id: InstallId,
    flag_evaluator: FlagEvaluator,
}

impl FeatureGate {
    pub fn new(policy: Arc<PolicyResolver>) -> Self {
        let install_id = InstallId::load_or_create();
        Self::new_with_install_id(policy, install_id)
    }

    pub fn new_with_install_id(policy: Arc<PolicyResolver>, install_id: InstallId) -> Self {
        let flag_evaluator = FlagEvaluator::new(install_id.clone());
        let snapshot = Self::build_community_snapshot(&policy);
        Self {
            snapshot: ArcSwap::new(Arc::new(snapshot)),
            policy,
            install_id,
            flag_evaluator,
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

    pub fn is_flag_enabled(&self, key: FlagKey) -> Option<bool> {
        let snap = self.snapshot.load();
        let flag = snap.flags.get(&key)?;
        Some(self.flag_evaluator.evaluate(key, flag, snap.tier))
    }

    pub fn update_state(&self, state: &LicenseState) {
        let new_snapshot = self.build_snapshot(state);
        self.snapshot.store(Arc::new(new_snapshot));
    }

    fn build_community_snapshot(policy: &PolicyResolver) -> EntitlementSnapshot {
        let features = Self::resolve_feature_keys(policy, "community");

        let quotas = Self::merge_quotas(Tier::Community, policy.document());
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
            Self::resolve_feature_keys(&self.policy, "community")
        } else {
            state
                .features
                .iter()
                .filter_map(|s| FeatureKey::from_known(s))
                .collect()
        };

        let quotas = Self::merge_quotas(tier, self.policy.document());
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

    /// Merge quota values for a tier.
    ///
    /// Compatibility defaults apply first as the baseline; policy quotas
    /// overlay on top, overwriting only keys explicitly declared. This
    /// guarantees baseline quotas are always present even with partial
    /// policies.
    fn merge_quotas(tier: Tier, policy_doc: Arc<PolicyDocument>) -> HashMap<QuotaKey, QuotaValue> {
        let mut quotas = HashMap::new();
        Self::apply_compatibility_quota_defaults(tier, &mut quotas);

        let tier_label = tier.label().to_lowercase();
        if let Some(tp) = policy_doc.tier_policies.get(&tier_label) {
            for (key_str, val) in &tp.quotas {
                if let Some(qk) = QuotaKey::from_known(key_str) {
                    if let Some(qv) = parse_quota_value(val) {
                        quotas.insert(qk, qv);
                    }
                }
            }
        }

        quotas
    }

    fn apply_compatibility_quota_defaults(tier: Tier, quotas: &mut HashMap<QuotaKey, QuotaValue>) {
        match tier {
            Tier::Community => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(3));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(128 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(1));
            }
            Tier::Pro => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(5));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(1024 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(3));
            }
            Tier::Team => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(10));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(10 * 1024 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(3));
                quotas.insert(QuotaKey::MinSeats, QuotaValue::Count(3));
            }
            Tier::Enterprise => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Unlimited);
            }
        }
    }

    fn resolve_feature_keys(policy: &PolicyResolver, tier: &str) -> AHashSet<FeatureKey> {
        policy
            .tier_features(tier)
            .unwrap_or_else(|err| {
                tracing::warn!("policy tier {tier:?} malformed: {err}; using empty features");
                BTreeSet::new()
            })
            .into_iter()
            .filter_map(|s| FeatureKey::from_known(&s))
            .collect()
    }

    fn extract_flags(doc: Arc<PolicyDocument>) -> HashMap<FlagKey, FlagSpec> {
        doc.flags
            .iter()
            .map(|(&key, spec)| (key, spec.clone()))
            .collect()
    }
}

/// Parse a raw JSON quota value from the signed policy document into the
/// typed `QuotaValue` enum. Returns `None` for unrecognized shapes so the
/// hardcoded default is preserved.
fn parse_quota_value(val: &serde_json::Value) -> Option<QuotaValue> {
    match val {
        serde_json::Value::Number(n) => n.as_u64().map(QuotaValue::Count),
        serde_json::Value::String(s) if s == "unlimited" => Some(QuotaValue::Unlimited),
        serde_json::Value::Object(map) => map
            .get("bytes")
            .and_then(|v| v.as_u64())
            .map(QuotaValue::Bytes)
            .or_else(|| {
                map.get("count")
                    .and_then(|v| v.as_u64())
                    .map(QuotaValue::Count)
            }),
        _ => None,
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

    #[test]
    fn parse_quota_value_accepts_unlimited_string() {
        assert_eq!(
            parse_quota_value(&serde_json::json!("unlimited")),
            Some(QuotaValue::Unlimited)
        );
    }
}
