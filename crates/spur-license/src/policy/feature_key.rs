//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.

use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

/// Const-compatible byte-slice equality (stable Rust).
const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

impl FeatureKey {
    // --- Community tier ---
    pub const CHAT: Self = Self("chat");
    pub const CODE_EDIT: Self = Self("code_edit");
    pub const WATCH_LOOP: Self = Self("watch_loop");
    pub const INLINE_COMPLETIONS: Self = Self("inline_completions");
    pub const BASIC_SEARCH: Self = Self("basic_search");
    pub const FILE_NAVIGATOR: Self = Self("file_navigator");
    pub const SYNTAX_HIGHLIGHT: Self = Self("syntax_highlight");
    pub const TERMINAL_INTEGRATION: Self = Self("terminal_integration");

    // --- Pro tier ---
    pub const ADVANCED_AGENTS: Self = Self("advanced_agents");
    pub const TEAM_SHARING: Self = Self("team_sharing");
    pub const CLOUD_SYNC: Self = Self("cloud_sync");
    pub const CUSTOM_AGENTS: Self = Self("custom_agents");
    pub const MULTI_FILE_EDIT: Self = Self("multi_file_edit");
    pub const CODE_REVIEW: Self = Self("code_review");
    pub const SMART_REFACTOR: Self = Self("smart_refactor");

    // --- Team tier ---
    pub const ORG_ADMIN: Self = Self("org_admin");
    pub const SEAT_MANAGEMENT: Self = Self("seat_management");
    pub const USAGE_ANALYTICS: Self = Self("usage_analytics");
    pub const AUDIT_LOGS: Self = Self("audit_logs");
    pub const SSO_INTEGRATION: Self = Self("sso_integration");
    pub const TEAM_TEMPLATES: Self = Self("team_templates");

    // --- Enterprise tier ---
    pub const DEDICATED_HOSTING: Self = Self("dedicated_hosting");
    pub const SLA_GUARANTEE: Self = Self("sla_guarantee");
    pub const CUSTOM_MODELS: Self = Self("custom_models");
    pub const PRIVATE_DEPLOYMENT: Self = Self("private_deployment");
    pub const ADVANCED_SECURITY: Self = Self("advanced_security");
    pub const COMPLIANCE_REPORTING: Self = Self("compliance_reporting");

    // --- G2 flag keys ---
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    /// Parse a known feature key from its string representation.
    /// Returns `None` for unknown strings — this is intentional to avoid
    /// shadowing `std::str::FromStr` and to make the "must be in registry"
    /// invariant explicit.
    pub const fn from_known(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if bytes_eq(b, b"chat") {
            Some(Self::CHAT)
        } else if bytes_eq(b, b"code_edit") {
            Some(Self::CODE_EDIT)
        } else if bytes_eq(b, b"watch_loop") {
            Some(Self::WATCH_LOOP)
        } else if bytes_eq(b, b"inline_completions") {
            Some(Self::INLINE_COMPLETIONS)
        } else if bytes_eq(b, b"basic_search") {
            Some(Self::BASIC_SEARCH)
        } else if bytes_eq(b, b"file_navigator") {
            Some(Self::FILE_NAVIGATOR)
        } else if bytes_eq(b, b"syntax_highlight") {
            Some(Self::SYNTAX_HIGHLIGHT)
        } else if bytes_eq(b, b"terminal_integration") {
            Some(Self::TERMINAL_INTEGRATION)
        } else if bytes_eq(b, b"advanced_agents") {
            Some(Self::ADVANCED_AGENTS)
        } else if bytes_eq(b, b"team_sharing") {
            Some(Self::TEAM_SHARING)
        } else if bytes_eq(b, b"cloud_sync") {
            Some(Self::CLOUD_SYNC)
        } else if bytes_eq(b, b"custom_agents") {
            Some(Self::CUSTOM_AGENTS)
        } else if bytes_eq(b, b"multi_file_edit") {
            Some(Self::MULTI_FILE_EDIT)
        } else if bytes_eq(b, b"code_review") {
            Some(Self::CODE_REVIEW)
        } else if bytes_eq(b, b"smart_refactor") {
            Some(Self::SMART_REFACTOR)
        } else if bytes_eq(b, b"org_admin") {
            Some(Self::ORG_ADMIN)
        } else if bytes_eq(b, b"seat_management") {
            Some(Self::SEAT_MANAGEMENT)
        } else if bytes_eq(b, b"usage_analytics") {
            Some(Self::USAGE_ANALYTICS)
        } else if bytes_eq(b, b"audit_logs") {
            Some(Self::AUDIT_LOGS)
        } else if bytes_eq(b, b"sso_integration") {
            Some(Self::SSO_INTEGRATION)
        } else if bytes_eq(b, b"team_templates") {
            Some(Self::TEAM_TEMPLATES)
        } else if bytes_eq(b, b"dedicated_hosting") {
            Some(Self::DEDICATED_HOSTING)
        } else if bytes_eq(b, b"sla_guarantee") {
            Some(Self::SLA_GUARANTEE)
        } else if bytes_eq(b, b"custom_models") {
            Some(Self::CUSTOM_MODELS)
        } else if bytes_eq(b, b"private_deployment") {
            Some(Self::PRIVATE_DEPLOYMENT)
        } else if bytes_eq(b, b"advanced_security") {
            Some(Self::ADVANCED_SECURITY)
        } else if bytes_eq(b, b"compliance_reporting") {
            Some(Self::COMPLIANCE_REPORTING)
        } else if bytes_eq(b, b"kill_advanced_planner") {
            Some(Self::KILL_ADVANCED_PLANNER)
        } else if bytes_eq(b, b"enable_browser_tool") {
            Some(Self::ENABLE_BROWSER_TOOL)
        } else if bytes_eq(b, b"enable_compaction_v2") {
            Some(Self::ENABLE_COMPACTION_V2)
        } else if bytes_eq(b, b"enable_telemetry") {
            Some(Self::ENABLE_TELEMETRY)
        } else {
            None
        }
    }
}

impl std::fmt::Display for FeatureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Newtype for feature keys that are NOT in the canonical registry.
/// Carries the raw string so callers can log/inspect it without losing
/// the original value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnknownFeatureKey(Arc<str>);

impl UnknownFeatureKey {
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UnknownFeatureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_const_name_lowercase() {
        assert_eq!(FeatureKey::CHAT.as_str(), "chat");
        assert_eq!(FeatureKey::ADVANCED_AGENTS.as_str(), "advanced_agents");
        assert_eq!(FeatureKey::ORG_ADMIN.as_str(), "org_admin");
        assert_eq!(FeatureKey::DEDICATED_HOSTING.as_str(), "dedicated_hosting");
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

    #[test]
    fn from_known_hits_all_31_keys() {
        // Community (8)
        assert_eq!(FeatureKey::from_known("chat"), Some(FeatureKey::CHAT));
        assert_eq!(FeatureKey::from_known("code_edit"), Some(FeatureKey::CODE_EDIT));
        assert_eq!(FeatureKey::from_known("watch_loop"), Some(FeatureKey::WATCH_LOOP));
        assert_eq!(
            FeatureKey::from_known("inline_completions"),
            Some(FeatureKey::INLINE_COMPLETIONS)
        );
        assert_eq!(
            FeatureKey::from_known("basic_search"),
            Some(FeatureKey::BASIC_SEARCH)
        );
        assert_eq!(
            FeatureKey::from_known("file_navigator"),
            Some(FeatureKey::FILE_NAVIGATOR)
        );
        assert_eq!(
            FeatureKey::from_known("syntax_highlight"),
            Some(FeatureKey::SYNTAX_HIGHLIGHT)
        );
        assert_eq!(
            FeatureKey::from_known("terminal_integration"),
            Some(FeatureKey::TERMINAL_INTEGRATION)
        );
        // Pro (7)
        assert_eq!(
            FeatureKey::from_known("advanced_agents"),
            Some(FeatureKey::ADVANCED_AGENTS)
        );
        assert_eq!(
            FeatureKey::from_known("team_sharing"),
            Some(FeatureKey::TEAM_SHARING)
        );
        assert_eq!(
            FeatureKey::from_known("cloud_sync"),
            Some(FeatureKey::CLOUD_SYNC)
        );
        assert_eq!(
            FeatureKey::from_known("custom_agents"),
            Some(FeatureKey::CUSTOM_AGENTS)
        );
        assert_eq!(
            FeatureKey::from_known("multi_file_edit"),
            Some(FeatureKey::MULTI_FILE_EDIT)
        );
        assert_eq!(
            FeatureKey::from_known("code_review"),
            Some(FeatureKey::CODE_REVIEW)
        );
        assert_eq!(
            FeatureKey::from_known("smart_refactor"),
            Some(FeatureKey::SMART_REFACTOR)
        );
        // Team (6)
        assert_eq!(
            FeatureKey::from_known("org_admin"),
            Some(FeatureKey::ORG_ADMIN)
        );
        assert_eq!(
            FeatureKey::from_known("seat_management"),
            Some(FeatureKey::SEAT_MANAGEMENT)
        );
        assert_eq!(
            FeatureKey::from_known("usage_analytics"),
            Some(FeatureKey::USAGE_ANALYTICS)
        );
        assert_eq!(
            FeatureKey::from_known("audit_logs"),
            Some(FeatureKey::AUDIT_LOGS)
        );
        assert_eq!(
            FeatureKey::from_known("sso_integration"),
            Some(FeatureKey::SSO_INTEGRATION)
        );
        assert_eq!(
            FeatureKey::from_known("team_templates"),
            Some(FeatureKey::TEAM_TEMPLATES)
        );
        // Enterprise (6)
        assert_eq!(
            FeatureKey::from_known("dedicated_hosting"),
            Some(FeatureKey::DEDICATED_HOSTING)
        );
        assert_eq!(
            FeatureKey::from_known("sla_guarantee"),
            Some(FeatureKey::SLA_GUARANTEE)
        );
        assert_eq!(
            FeatureKey::from_known("custom_models"),
            Some(FeatureKey::CUSTOM_MODELS)
        );
        assert_eq!(
            FeatureKey::from_known("private_deployment"),
            Some(FeatureKey::PRIVATE_DEPLOYMENT)
        );
        assert_eq!(
            FeatureKey::from_known("advanced_security"),
            Some(FeatureKey::ADVANCED_SECURITY)
        );
        assert_eq!(
            FeatureKey::from_known("compliance_reporting"),
            Some(FeatureKey::COMPLIANCE_REPORTING)
        );
        // G2 flags (4)
        assert_eq!(
            FeatureKey::from_known("kill_advanced_planner"),
            Some(FeatureKey::KILL_ADVANCED_PLANNER)
        );
        assert_eq!(
            FeatureKey::from_known("enable_browser_tool"),
            Some(FeatureKey::ENABLE_BROWSER_TOOL)
        );
        assert_eq!(
            FeatureKey::from_known("enable_compaction_v2"),
            Some(FeatureKey::ENABLE_COMPACTION_V2)
        );
        assert_eq!(
            FeatureKey::from_known("enable_telemetry"),
            Some(FeatureKey::ENABLE_TELEMETRY)
        );
    }

    #[test]
    fn from_known_returns_none_for_unknown() {
        assert_eq!(FeatureKey::from_known("not_a_feature"), None);
        assert_eq!(FeatureKey::from_known(""), None);
        assert_eq!(FeatureKey::from_known("Chat"), None); // case-sensitive
    }

    #[test]
    fn unknown_feature_key_display_and_access() {
        let unk = UnknownFeatureKey::new("experimental_thing");
        assert_eq!(unk.as_str(), "experimental_thing");
        assert_eq!(format!("{}", unk), "experimental_thing");
    }
}
