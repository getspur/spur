//! Typed const registry for G2 runtime flag keys.
//!
//! Flags share the signed policy document with tier entitlements, but they are
//! a separate namespace from `FeatureKey`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlagKey(&'static str);

pub const KILL_ADVANCED_PLANNER: FlagKey = FlagKey("kill_advanced_planner");
pub const ENABLE_BROWSER_TOOL: FlagKey = FlagKey("enable_browser_tool");
pub const ENABLE_COMPACTION_V2: FlagKey = FlagKey("enable_compaction_v2");
pub const ENABLE_TELEMETRY: FlagKey = FlagKey("enable_telemetry");

impl FlagKey {
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    pub fn from_known(s: &str) -> Option<Self> {
        match s {
            "kill_advanced_planner" => Some(Self::KILL_ADVANCED_PLANNER),
            "enable_browser_tool" => Some(Self::ENABLE_BROWSER_TOOL),
            "enable_compaction_v2" => Some(Self::ENABLE_COMPACTION_V2),
            "enable_telemetry" => Some(Self::ENABLE_TELEMETRY),
            _ => None,
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

impl<'de> Deserialize<'de> for FlagKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_known(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown policy flag key {raw:?}")))
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
            Some(KILL_ADVANCED_PLANNER)
        );
        assert_eq!(
            FlagKey::from_known("enable_browser_tool"),
            Some(ENABLE_BROWSER_TOOL)
        );
        assert_eq!(
            FlagKey::from_known("enable_compaction_v2"),
            Some(ENABLE_COMPACTION_V2)
        );
        assert_eq!(
            FlagKey::from_known("enable_telemetry"),
            Some(ENABLE_TELEMETRY)
        );
    }

    #[test]
    fn from_known_returns_none_for_unknown() {
        assert_eq!(FlagKey::from_known("brain_session"), None);
        assert_eq!(FlagKey::from_known(""), None);
    }
}
