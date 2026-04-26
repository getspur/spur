//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.
//!
//! ## Naming convention (post-2026-04-26 tier revamp)
//!
//! New keys follow `<crate>_<tier>_<capability>` where:
//! - `<crate>` ∈ {acp, core, mcp, tui, cli, pm, cost, worktree, license, bot,
//!   interactive, blob, ctx, skills, notif}
//! - `<tier>` ∈ {core (Free baseline), pro (Pro upsell), team (Team v2-deferred)}
//! - `<capability>` is a single atomic capability, lowercase snake_case
//!
//! Const name is UPPER_SNAKE_CASE of the underlying string. Grep
//! `pm_pro_*` to find every Pro PM gate. The legacy keys above (BRAIN_SESSION
//! etc.) remain during the v0 → v1 transition; Plan B removes them after
//! callers migrate.
//!
//! See `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
//! §4 for the full 135-key registry.

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
    // --- Community tier (11) ---
    pub const BRAIN_SESSION: Self = Self("brain_session");
    pub const SINGLE_WORKER: Self = Self("single_worker");
    pub const WORKTREE_ISOLATION: Self = Self("worktree_isolation");
    pub const MANUAL_REVIEW: Self = Self("manual_review");
    pub const EVENT_PERSISTENCE: Self = Self("event_persistence");
    pub const BASIC_LINEAGE: Self = Self("basic_lineage");
    pub const TUI_DASHBOARD: Self = Self("tui_dashboard");
    pub const BASIC_COST_DISPLAY: Self = Self("basic_cost_display");
    pub const BASIC_NOTIFICATIONS: Self = Self("basic_notifications");
    pub const LOCAL_CONFIG: Self = Self("local_config");
    pub const MCP_STANDARD_TOOLS: Self = Self("mcp_standard_tools");

    // --- Pro tier (8) ---
    pub const PARALLEL_WORKERS: Self = Self("parallel_workers");
    pub const AUTO_REVIEW_POLICIES: Self = Self("auto_review_policies");
    pub const SESSION_RESUME: Self = Self("session_resume");
    pub const ADVANCED_COST_ANALYTICS: Self = Self("advanced_cost_analytics");
    pub const CUSTOM_WORKTREE_POLICIES: Self = Self("custom_worktree_policies");
    pub const CUSTOM_NOTIFICATIONS: Self = Self("custom_notifications");
    pub const EXTENDED_RETENTION: Self = Self("extended_retention");
    pub const TUI_SESSION_DETAIL: Self = Self("tui_session_detail");

    // --- Team tier (7) ---
    pub const PM_INTEGRATION: Self = Self("pm_integration");
    pub const SHARED_LINEAGE: Self = Self("shared_lineage");
    pub const TEAM_COST_DASHBOARD: Self = Self("team_cost_dashboard");
    pub const CENTRALIZED_CONFIG: Self = Self("centralized_config");
    pub const RBAC: Self = Self("rbac");
    pub const SHARED_REVIEW_QUEUE: Self = Self("shared_review_queue");
    pub const PM_WEBHOOKS: Self = Self("pm_webhooks");

    // --- Enterprise tier (6) ---
    pub const SSO_SAML: Self = Self("sso_saml");
    pub const AUDIT_LOGS: Self = Self("audit_logs");
    pub const CUSTOM_POLICIES: Self = Self("custom_policies");
    pub const CUSTOM_MCP_TOOLS: Self = Self("custom_mcp_tools");
    pub const DEDICATED_SUPPORT: Self = Self("dedicated_support");
    pub const SLA_GUARANTEE: Self = Self("sla_guarantee");

    // --- G2 flag keys (4) ---
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    // === Tier revamp v1 keys (post-2026-04-26) ===

    // --- spur-acp (11) ---
    pub const ACP_CORE_TRANSPORT_STDIO: Self = Self("acp_core_transport_stdio");
    pub const ACP_CORE_TRANSPORT_SOCKET: Self = Self("acp_core_transport_socket");
    pub const ACP_CORE_ADAPTER_CLAUDE_CODE: Self = Self("acp_core_adapter_claude_code");
    pub const ACP_CORE_ADAPTER_CODEX: Self = Self("acp_core_adapter_codex");
    pub const ACP_CORE_ADAPTER_GEMINI: Self = Self("acp_core_adapter_gemini");
    pub const ACP_CORE_ADAPTER_KIRO: Self = Self("acp_core_adapter_kiro");
    pub const ACP_CORE_ADAPTER_CURSOR: Self = Self("acp_core_adapter_cursor");
    pub const ACP_CORE_ADAPTER_OPENCODE: Self = Self("acp_core_adapter_opencode");
    pub const ACP_CORE_ADAPTER_KIMI: Self = Self("acp_core_adapter_kimi");
    pub const ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK: Self =
        Self("acp_core_session_attach_advisory_lock");
    pub const ACP_CORE_SESSION_ATTACH_DEGRADED_NOLOCK: Self =
        Self("acp_core_session_attach_degraded_nolock");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    /// Parse a known feature key from its string representation.
    /// Returns `None` for unknown strings — this is intentional to avoid
    /// shadowing `std::str::FromStr` and to make the "must be in registry"
    /// invariant explicit.
    pub const fn from_known(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if bytes_eq(b, b"brain_session") {
            Some(Self::BRAIN_SESSION)
        } else if bytes_eq(b, b"single_worker") {
            Some(Self::SINGLE_WORKER)
        } else if bytes_eq(b, b"worktree_isolation") {
            Some(Self::WORKTREE_ISOLATION)
        } else if bytes_eq(b, b"manual_review") {
            Some(Self::MANUAL_REVIEW)
        } else if bytes_eq(b, b"event_persistence") {
            Some(Self::EVENT_PERSISTENCE)
        } else if bytes_eq(b, b"basic_lineage") {
            Some(Self::BASIC_LINEAGE)
        } else if bytes_eq(b, b"tui_dashboard") {
            Some(Self::TUI_DASHBOARD)
        } else if bytes_eq(b, b"basic_cost_display") {
            Some(Self::BASIC_COST_DISPLAY)
        } else if bytes_eq(b, b"basic_notifications") {
            Some(Self::BASIC_NOTIFICATIONS)
        } else if bytes_eq(b, b"local_config") {
            Some(Self::LOCAL_CONFIG)
        } else if bytes_eq(b, b"mcp_standard_tools") {
            Some(Self::MCP_STANDARD_TOOLS)
        } else if bytes_eq(b, b"parallel_workers") {
            Some(Self::PARALLEL_WORKERS)
        } else if bytes_eq(b, b"auto_review_policies") {
            Some(Self::AUTO_REVIEW_POLICIES)
        } else if bytes_eq(b, b"session_resume") {
            Some(Self::SESSION_RESUME)
        } else if bytes_eq(b, b"advanced_cost_analytics") {
            Some(Self::ADVANCED_COST_ANALYTICS)
        } else if bytes_eq(b, b"custom_worktree_policies") {
            Some(Self::CUSTOM_WORKTREE_POLICIES)
        } else if bytes_eq(b, b"custom_notifications") {
            Some(Self::CUSTOM_NOTIFICATIONS)
        } else if bytes_eq(b, b"extended_retention") {
            Some(Self::EXTENDED_RETENTION)
        } else if bytes_eq(b, b"tui_session_detail") {
            Some(Self::TUI_SESSION_DETAIL)
        } else if bytes_eq(b, b"pm_integration") {
            Some(Self::PM_INTEGRATION)
        } else if bytes_eq(b, b"shared_lineage") {
            Some(Self::SHARED_LINEAGE)
        } else if bytes_eq(b, b"team_cost_dashboard") {
            Some(Self::TEAM_COST_DASHBOARD)
        } else if bytes_eq(b, b"centralized_config") {
            Some(Self::CENTRALIZED_CONFIG)
        } else if bytes_eq(b, b"rbac") {
            Some(Self::RBAC)
        } else if bytes_eq(b, b"shared_review_queue") {
            Some(Self::SHARED_REVIEW_QUEUE)
        } else if bytes_eq(b, b"pm_webhooks") {
            Some(Self::PM_WEBHOOKS)
        } else if bytes_eq(b, b"sso_saml") {
            Some(Self::SSO_SAML)
        } else if bytes_eq(b, b"audit_logs") {
            Some(Self::AUDIT_LOGS)
        } else if bytes_eq(b, b"custom_policies") {
            Some(Self::CUSTOM_POLICIES)
        } else if bytes_eq(b, b"custom_mcp_tools") {
            Some(Self::CUSTOM_MCP_TOOLS)
        } else if bytes_eq(b, b"dedicated_support") {
            Some(Self::DEDICATED_SUPPORT)
        } else if bytes_eq(b, b"sla_guarantee") {
            Some(Self::SLA_GUARANTEE)
        } else if bytes_eq(b, b"kill_advanced_planner") {
            Some(Self::KILL_ADVANCED_PLANNER)
        } else if bytes_eq(b, b"enable_browser_tool") {
            Some(Self::ENABLE_BROWSER_TOOL)
        } else if bytes_eq(b, b"enable_compaction_v2") {
            Some(Self::ENABLE_COMPACTION_V2)
        } else if bytes_eq(b, b"enable_telemetry") {
            Some(Self::ENABLE_TELEMETRY)
        // ===== Tier revamp v1 keys =====
        // spur-acp
        } else if bytes_eq(b, b"acp_core_transport_stdio") {
            Some(Self::ACP_CORE_TRANSPORT_STDIO)
        } else if bytes_eq(b, b"acp_core_transport_socket") {
            Some(Self::ACP_CORE_TRANSPORT_SOCKET)
        } else if bytes_eq(b, b"acp_core_adapter_claude_code") {
            Some(Self::ACP_CORE_ADAPTER_CLAUDE_CODE)
        } else if bytes_eq(b, b"acp_core_adapter_codex") {
            Some(Self::ACP_CORE_ADAPTER_CODEX)
        } else if bytes_eq(b, b"acp_core_adapter_gemini") {
            Some(Self::ACP_CORE_ADAPTER_GEMINI)
        } else if bytes_eq(b, b"acp_core_adapter_kiro") {
            Some(Self::ACP_CORE_ADAPTER_KIRO)
        } else if bytes_eq(b, b"acp_core_adapter_cursor") {
            Some(Self::ACP_CORE_ADAPTER_CURSOR)
        } else if bytes_eq(b, b"acp_core_adapter_opencode") {
            Some(Self::ACP_CORE_ADAPTER_OPENCODE)
        } else if bytes_eq(b, b"acp_core_adapter_kimi") {
            Some(Self::ACP_CORE_ADAPTER_KIMI)
        } else if bytes_eq(b, b"acp_core_session_attach_advisory_lock") {
            Some(Self::ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK)
        } else if bytes_eq(b, b"acp_core_session_attach_degraded_nolock") {
            Some(Self::ACP_CORE_SESSION_ATTACH_DEGRADED_NOLOCK)
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
        assert_eq!(FeatureKey::BRAIN_SESSION.as_str(), "brain_session");
        assert_eq!(FeatureKey::PARALLEL_WORKERS.as_str(), "parallel_workers");
        assert_eq!(FeatureKey::PM_INTEGRATION.as_str(), "pm_integration");
        assert_eq!(FeatureKey::SSO_SAML.as_str(), "sso_saml");
        assert_eq!(
            FeatureKey::KILL_ADVANCED_PLANNER.as_str(),
            "kill_advanced_planner"
        );
    }

    #[test]
    fn copy_eq_and_hash_work() {
        let a = FeatureKey::BRAIN_SESSION;
        let b = FeatureKey::BRAIN_SESSION;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn from_known_hits_all_36_keys() {
        // Community (11)
        assert_eq!(
            FeatureKey::from_known("brain_session"),
            Some(FeatureKey::BRAIN_SESSION)
        );
        assert_eq!(
            FeatureKey::from_known("single_worker"),
            Some(FeatureKey::SINGLE_WORKER)
        );
        assert_eq!(
            FeatureKey::from_known("worktree_isolation"),
            Some(FeatureKey::WORKTREE_ISOLATION)
        );
        assert_eq!(
            FeatureKey::from_known("manual_review"),
            Some(FeatureKey::MANUAL_REVIEW)
        );
        assert_eq!(
            FeatureKey::from_known("event_persistence"),
            Some(FeatureKey::EVENT_PERSISTENCE)
        );
        assert_eq!(
            FeatureKey::from_known("basic_lineage"),
            Some(FeatureKey::BASIC_LINEAGE)
        );
        assert_eq!(
            FeatureKey::from_known("tui_dashboard"),
            Some(FeatureKey::TUI_DASHBOARD)
        );
        assert_eq!(
            FeatureKey::from_known("basic_cost_display"),
            Some(FeatureKey::BASIC_COST_DISPLAY)
        );
        assert_eq!(
            FeatureKey::from_known("basic_notifications"),
            Some(FeatureKey::BASIC_NOTIFICATIONS)
        );
        assert_eq!(
            FeatureKey::from_known("local_config"),
            Some(FeatureKey::LOCAL_CONFIG)
        );
        assert_eq!(
            FeatureKey::from_known("mcp_standard_tools"),
            Some(FeatureKey::MCP_STANDARD_TOOLS)
        );
        // Pro (8)
        assert_eq!(
            FeatureKey::from_known("parallel_workers"),
            Some(FeatureKey::PARALLEL_WORKERS)
        );
        assert_eq!(
            FeatureKey::from_known("auto_review_policies"),
            Some(FeatureKey::AUTO_REVIEW_POLICIES)
        );
        assert_eq!(
            FeatureKey::from_known("session_resume"),
            Some(FeatureKey::SESSION_RESUME)
        );
        assert_eq!(
            FeatureKey::from_known("advanced_cost_analytics"),
            Some(FeatureKey::ADVANCED_COST_ANALYTICS)
        );
        assert_eq!(
            FeatureKey::from_known("custom_worktree_policies"),
            Some(FeatureKey::CUSTOM_WORKTREE_POLICIES)
        );
        assert_eq!(
            FeatureKey::from_known("custom_notifications"),
            Some(FeatureKey::CUSTOM_NOTIFICATIONS)
        );
        assert_eq!(
            FeatureKey::from_known("extended_retention"),
            Some(FeatureKey::EXTENDED_RETENTION)
        );
        assert_eq!(
            FeatureKey::from_known("tui_session_detail"),
            Some(FeatureKey::TUI_SESSION_DETAIL)
        );
        // Team (7)
        assert_eq!(
            FeatureKey::from_known("pm_integration"),
            Some(FeatureKey::PM_INTEGRATION)
        );
        assert_eq!(
            FeatureKey::from_known("shared_lineage"),
            Some(FeatureKey::SHARED_LINEAGE)
        );
        assert_eq!(
            FeatureKey::from_known("team_cost_dashboard"),
            Some(FeatureKey::TEAM_COST_DASHBOARD)
        );
        assert_eq!(
            FeatureKey::from_known("centralized_config"),
            Some(FeatureKey::CENTRALIZED_CONFIG)
        );
        assert_eq!(FeatureKey::from_known("rbac"), Some(FeatureKey::RBAC));
        assert_eq!(
            FeatureKey::from_known("shared_review_queue"),
            Some(FeatureKey::SHARED_REVIEW_QUEUE)
        );
        assert_eq!(
            FeatureKey::from_known("pm_webhooks"),
            Some(FeatureKey::PM_WEBHOOKS)
        );
        // Enterprise (6)
        assert_eq!(
            FeatureKey::from_known("sso_saml"),
            Some(FeatureKey::SSO_SAML)
        );
        assert_eq!(
            FeatureKey::from_known("audit_logs"),
            Some(FeatureKey::AUDIT_LOGS)
        );
        assert_eq!(
            FeatureKey::from_known("custom_policies"),
            Some(FeatureKey::CUSTOM_POLICIES)
        );
        assert_eq!(
            FeatureKey::from_known("custom_mcp_tools"),
            Some(FeatureKey::CUSTOM_MCP_TOOLS)
        );
        assert_eq!(
            FeatureKey::from_known("dedicated_support"),
            Some(FeatureKey::DEDICATED_SUPPORT)
        );
        assert_eq!(
            FeatureKey::from_known("sla_guarantee"),
            Some(FeatureKey::SLA_GUARANTEE)
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
        assert_eq!(FeatureKey::from_known("Brain_Session"), None); // case-sensitive
    }

    #[test]
    fn unknown_feature_key_display_and_access() {
        let unk = UnknownFeatureKey::new("experimental_thing");
        assert_eq!(unk.as_str(), "experimental_thing");
        assert_eq!(format!("{}", unk), "experimental_thing");
    }

    /// Guards against accidental removal of registered keys.
    /// Bump the expected count when adding new keys via dedicated tasks.
    #[test]
    fn registered_key_count_matches_expected() {
        const EXPECTED_TOTAL_KEYS: usize = 36;
        let mut count = 0usize;
        for s in &[
            // Community (11)
            "brain_session",
            "single_worker",
            "worktree_isolation",
            "manual_review",
            "event_persistence",
            "basic_lineage",
            "tui_dashboard",
            "basic_cost_display",
            "basic_notifications",
            "local_config",
            "mcp_standard_tools",
            // Pro (8)
            "parallel_workers",
            "auto_review_policies",
            "session_resume",
            "advanced_cost_analytics",
            "custom_worktree_policies",
            "custom_notifications",
            "extended_retention",
            "tui_session_detail",
            // Team (7)
            "pm_integration",
            "shared_lineage",
            "team_cost_dashboard",
            "centralized_config",
            "rbac",
            "shared_review_queue",
            "pm_webhooks",
            // Enterprise (6)
            "sso_saml",
            "audit_logs",
            "custom_policies",
            "custom_mcp_tools",
            "dedicated_support",
            "sla_guarantee",
            // G2 flags (4)
            "kill_advanced_planner",
            "enable_browser_tool",
            "enable_compaction_v2",
            "enable_telemetry",
        ] {
            assert!(
                FeatureKey::from_known(s).is_some(),
                "key {s:?} not parseable",
            );
            count += 1;
        }
        assert_eq!(count, EXPECTED_TOTAL_KEYS, "key count mismatch");
    }

    #[test]
    fn spur_acp_keys_registered() {
        for s in &[
            "acp_core_transport_stdio",
            "acp_core_transport_socket",
            "acp_core_adapter_claude_code",
            "acp_core_adapter_codex",
            "acp_core_adapter_gemini",
            "acp_core_adapter_kiro",
            "acp_core_adapter_cursor",
            "acp_core_adapter_opencode",
            "acp_core_adapter_kimi",
            "acp_core_session_attach_advisory_lock",
            "acp_core_session_attach_degraded_nolock",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
}
