use std::collections::HashMap;

use ahash::AHashSet;
use chrono::{DateTime, Utc};

use crate::policy::{FeatureKey, FlagKey, FlagSpec};
use crate::quota::{QuotaKey, QuotaValue};
use crate::tier::Tier;
use crate::Plan;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementSnapshot {
    pub tier: Tier,
    pub features: AHashSet<FeatureKey>,
    pub quotas: HashMap<QuotaKey, QuotaValue>,
    pub flags: HashMap<FlagKey, FlagSpec>,
    pub source: SourceMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub plan: Plan,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_offline: bool,
}

impl Default for EntitlementSnapshot {
    fn default() -> Self {
        Self {
            tier: Tier::Community,
            features: AHashSet::default(),
            quotas: HashMap::new(),
            flags: HashMap::new(),
            source: SourceMetadata {
                plan: Plan::Community,
                expires_at: None,
                is_offline: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_is_community_with_no_features() {
        let snap = EntitlementSnapshot::default();
        assert_eq!(snap.tier, Tier::Community);
        assert!(snap.features.is_empty());
        assert!(snap.quotas.is_empty());
        assert!(snap.flags.is_empty());
        assert_eq!(snap.source.plan, Plan::Community);
        assert!(snap.source.expires_at.is_none());
        assert!(!snap.source.is_offline);
    }
}
