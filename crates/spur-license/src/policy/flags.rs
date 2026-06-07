use crate::install_id::InstallId;
use crate::policy::{FlagKey, FlagSpec};
use crate::tier::Tier;

/// G2 flag evaluator: kill switch, tier filter, deterministic rollout.
pub struct FlagEvaluator {
    install_id: InstallId,
}

impl FlagEvaluator {
    pub fn new(install_id: InstallId) -> Self {
        Self { install_id }
    }

    /// Evaluate whether a flag is enabled for the given tier.
    /// Deterministic: same (`install_id`, `flag_key`) always yields same result.
    pub fn evaluate(&self, key: FlagKey, flag: &FlagSpec, tier: Tier) -> bool {
        // 1. Kill switch
        if !flag.enabled {
            return false;
        }

        // 2. Tier filter
        if let Some(tiers) = flag.tier_filter.as_ref() {
            let tier_str = tier.label().to_lowercase();
            if !tiers.iter().any(|t| t == &tier_str) {
                return false;
            }
        }

        // 3. Rollout percentage (deterministic hash)
        if let Some(pct) = flag.rollout_percent {
            let hash = seahash::hash(format!("{}:{}", self.install_id, key.as_str()).as_bytes());
            let normalized = (hash % 100) as f32;
            return normalized < pct;
        }

        true
    }
}

/// Explanation of why a flag evaluated to its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct FlagExplanation {
    pub key: String,
    pub enabled: bool,
    pub reason: FlagReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlagReason {
    KillSwitch,
    TierFilter,
    Rollout { bucket: u8, percent: f32 },
    Enabled,
}
