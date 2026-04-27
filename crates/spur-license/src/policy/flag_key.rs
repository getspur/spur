//! Typed const registry for G2 runtime flag keys.
//!
//! Flags share the signed policy document with tier entitlements, but they are
//! a separate namespace from `FeatureKey`.

use serde::{Serialize, Serializer};

use super::const_eq::bytes_eq;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlagKey(&'static str);

impl FlagKey {
    /// Emergency kill switch for advanced planner rollout.
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    /// Controls browser tool exposure while rollout is gated.
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    /// Gates the second-generation context compaction path.
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    /// Enables runtime telemetry collection and reporting.
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");
    /// Exposes deferred v1.1 preview behavior behind a signed policy flag.
    pub const ENABLE_V1_1_PREVIEW: Self = Self("enable_v1_1_preview");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    pub const fn from_known(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if bytes_eq(b, b"kill_advanced_planner") {
            Some(Self::KILL_ADVANCED_PLANNER)
        } else if bytes_eq(b, b"enable_browser_tool") {
            Some(Self::ENABLE_BROWSER_TOOL)
        } else if bytes_eq(b, b"enable_compaction_v2") {
            Some(Self::ENABLE_COMPACTION_V2)
        } else if bytes_eq(b, b"enable_telemetry") {
            Some(Self::ENABLE_TELEMETRY)
        } else if bytes_eq(b, b"enable_v1_1_preview") {
            Some(Self::ENABLE_V1_1_PREVIEW)
        } else {
            None
        }
    }
}

impl Serialize for FlagKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0)
    }
}

impl std::fmt::Display for FlagKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_known_hits_all_flags() {
        assert_eq!(
            FlagKey::from_known("kill_advanced_planner"),
            Some(FlagKey::KILL_ADVANCED_PLANNER)
        );
        assert_eq!(
            FlagKey::from_known("enable_browser_tool"),
            Some(FlagKey::ENABLE_BROWSER_TOOL)
        );
        assert_eq!(
            FlagKey::from_known("enable_compaction_v2"),
            Some(FlagKey::ENABLE_COMPACTION_V2)
        );
        assert_eq!(
            FlagKey::from_known("enable_telemetry"),
            Some(FlagKey::ENABLE_TELEMETRY)
        );
        assert_eq!(
            FlagKey::from_known("enable_v1_1_preview"),
            Some(FlagKey::ENABLE_V1_1_PREVIEW)
        );
    }

    #[test]
    fn from_known_returns_none_for_unknown() {
        assert_eq!(FlagKey::from_known("brain_session"), None);
        assert_eq!(FlagKey::from_known(""), None);
    }
}
