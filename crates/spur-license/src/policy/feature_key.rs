//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    // G1 entitlement keys (referenced by `tier_policies[*].features`)
    pub const CHAT: Self = Self("chat");
    pub const CODE_EDIT: Self = Self("code_edit");
    pub const WATCH_LOOP: Self = Self("watch_loop");
    pub const ADVANCED_AGENTS: Self = Self("advanced_agents");
    pub const TEAM_SHARING: Self = Self("team_sharing");
    pub const CLOUD_SYNC: Self = Self("cloud_sync");

    // G2 flag keys (referenced by `flags[*]`)
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for FeatureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_const_name_lowercase() {
        assert_eq!(FeatureKey::ADVANCED_AGENTS.as_str(), "advanced_agents");
        assert_eq!(
            FeatureKey::KILL_ADVANCED_PLANNER.as_str(),
            "kill_advanced_planner"
        );
    }

    #[test]
    fn copy_eq_and_hash_work() {
        let a = FeatureKey::CHAT;
        let b = FeatureKey::CHAT;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }
}
