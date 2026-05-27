//! Typed const registry of G1 entitlement feature keys.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and `LicenseSeat` catalog speak; this newtype exists to
//! make callers typo-safe.
//!
//! ## Naming convention (post-2026-04-26 tier revamp)
//!
//! New keys follow `<crate>_<tier>_<capability>` where:
//! - `<crate>` ∈ {acp, core, mcp, tui, cli, pm, cost, worktree, license, bot,
//!   interactive, blob, ctx, skills, notif}
//! - `<tier>` ∈ {core (Free baseline), pro (Pro upsell), team (Team v2-deferred)}
//! - `<capability>` is a single atomic capability, lowercase `snake_case`
//!
//! Const name is `UPPER_SNAKE_CASE` of the underlying string. Grep
//! `pm_pro_*` to find every Pro PM gate.
//!
//! See `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
//! §4.15 for the Wave-9-final 63-key registry.

use super::const_eq::bytes_eq;
use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    // === Tier revamp v1 keys (post-2026-04-26, Wave-9 final shape: 63 keys) ===

    // --- spur-acp (6) — Wave 8: dropped 3 ghost adapters + merged degraded_nolock into advisory_lock; Wave 8.5: dropped gemini ghost adapter ---
    pub const ACP_CORE_TRANSPORT_STDIO: Self = Self("acp_core_transport_stdio");
    pub const ACP_CORE_TRANSPORT_SOCKET: Self = Self("acp_core_transport_socket");
    pub const ACP_CORE_ADAPTER_CLAUDE_CODE: Self = Self("acp_core_adapter_claude_code");
    pub const ACP_CORE_ADAPTER_CODEX: Self = Self("acp_core_adapter_codex");
    pub const ACP_CORE_ADAPTER_KIRO: Self = Self("acp_core_adapter_kiro");
    pub const ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK: Self =
        Self("acp_core_session_attach_advisory_lock");

    // --- spur-core: brain (2) — Wave 8: consolidated trio → brain_session; deferred auto_pool to v1.1 ---
    pub const CORE_CORE_BRAIN_SESSION: Self = Self("core_core_brain_session");
    pub const CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE: Self =
        Self("core_core_brain_failover_manual_keystroke");

    // --- spur-core: workers (2) — Wave 8: merged cancellable_semaphore into parallel_workers ---
    pub const CORE_CORE_PARALLEL_WORKERS: Self = Self("core_core_parallel_workers");
    pub const CORE_PRO_WORKER_HEARTBEAT_WATCHDOG: Self = Self("core_pro_worker_heartbeat_watchdog");

    // --- spur-core: event pipeline (1) — Wave 8 NEW umbrella: collapsed funnel+sink+lineage+pump+agent_notification+tui_drain → event_pipeline; deferred broadcast_lagged_recovery to v1.1 ---
    pub const CORE_CORE_EVENT_PIPELINE: Self = Self("core_core_event_pipeline");

    // --- spur-core: peer mailbox (1) — Wave 8: collapsed router+ledger+stranded_recon → router (constructor compile-coupled) ---
    pub const CORE_PRO_PEER_MAILBOX_ROUTER: Self = Self("core_pro_peer_mailbox_router");

    // --- spur-core: review (3) — Wave 8: collapsed sink+timeout+retry → review; merged timeout_routing into auto_approve.
    //     Wave 9: tier-shifted retry_config Pro→Free per "Free reliability baseline" precedent (max_review_retries config; backoff hard-coded).
    pub const CORE_CORE_REVIEW: Self = Self("core_core_review");
    pub const CORE_CORE_REVIEW_RETRY_CONFIG: Self = Self("core_core_review_retry_config");
    pub const CORE_PRO_REVIEW_AUTO_APPROVE: Self = Self("core_pro_review_auto_approve");

    // --- spur-core: system events (1) — Wave 8: deferred conflict + rate_limit (no production emitters); agent_notification absorbed by event_pipeline ---
    pub const CORE_CORE_PERMISSION_REQUEST_DETECTION: Self =
        Self("core_core_permission_request_detection");

    // --- spur-core: reliability & lifecycle (3) — Wave 8: merged plan_orphan_recovery into plan_persistence; dropped background_task_tracker (mechanism plumbing) ---
    pub const CORE_CORE_SESSION_RESUME: Self = Self("core_core_session_resume");
    pub const CORE_PRO_SESSION_RESUME_EVENT_REPLAY: Self =
        Self("core_pro_session_resume_event_replay");
    pub const CORE_CORE_PLAN_PERSISTENCE: Self = Self("core_core_plan_persistence");

    // --- skills (2) — Wave 8: consolidated registry+atomic_installation+render+role_gating → registry ---
    pub const SKILLS_CORE_REGISTRY: Self = Self("skills_core_registry");
    pub const SKILLS_PRO_CUSTOM: Self = Self("skills_pro_custom");

    // --- spur-mcp (10) — Wave 8: merged outcome_materializer into delegate, reconciler_journal_notify into plan_durable, mutation_executor into signal_watcher; deferred custom_tools to v1.1.
    //     Wave 9: tier-shifted graph_tools Pro→Free per "viral acquisition surface" rationale (raw JSON / Mermaid text output via `bv` graph passthrough).
    pub const MCP_CORE_SERVER_DISPATCH: Self = Self("mcp_core_server_dispatch");
    pub const MCP_CORE_DELEGATE: Self = Self("mcp_core_delegate");
    pub const MCP_CORE_OUTCOME_FETCH: Self = Self("mcp_core_outcome_fetch");
    pub const MCP_CORE_PM: Self = Self("mcp_core_pm");
    pub const MCP_CORE_PR: Self = Self("mcp_core_pr");
    pub const MCP_CORE_PLAN_EPHEMERAL: Self = Self("mcp_core_plan_ephemeral");
    pub const MCP_CORE_GRAPH_TOOLS: Self = Self("mcp_core_graph_tools");
    pub const MCP_PRO_PLAN_DURABLE: Self = Self("mcp_pro_plan_durable");
    pub const MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT: Self = Self("mcp_pro_signal_watcher_scope_drift");
    pub const MCP_PRO_REVIEW: Self = Self("mcp_pro_review");

    // --- spur-tui (7) — Wave 8: collapsed dashboard+landing+composer → dashboard; notification_drain absorbed by event_pipeline ---
    pub const TUI_CORE_VIEW_DASHBOARD: Self = Self("tui_core_view_dashboard");
    pub const TUI_CORE_VIEW_SESSION_DETAIL: Self = Self("tui_core_view_session_detail");
    pub const TUI_CORE_VIEW_PLAN_INSPECTOR: Self = Self("tui_core_view_plan_inspector");
    pub const TUI_CORE_VIEW_PALETTE_OVERLAY: Self = Self("tui_core_view_palette_overlay");
    pub const TUI_CORE_VIEW_ISSUE_BROWSER: Self = Self("tui_core_view_issue_browser");
    pub const TUI_CORE_MODAL_COLLISION_ESCAPE: Self = Self("tui_core_modal_collision_escape");
    pub const TUI_CORE_INPUT_PASTE_AS_ATOM: Self = Self("tui_core_input_paste_as_atom");

    // --- spur-cli (9) ---
    pub const CLI_CORE_INIT: Self = Self("cli_core_init");
    pub const CLI_CORE_AGENTS: Self = Self("cli_core_agents");
    pub const CLI_CORE_SESSIONS: Self = Self("cli_core_sessions");
    pub const CLI_CORE_RUN: Self = Self("cli_core_run");
    pub const CLI_CORE_EXEC: Self = Self("cli_core_exec");
    pub const CLI_CORE_TUI: Self = Self("cli_core_tui");
    pub const CLI_CORE_COST: Self = Self("cli_core_cost");
    pub const CLI_CORE_CONNECT: Self = Self("cli_core_connect");
    pub const CLI_CORE_LICENSE_ACTIVATE: Self = Self("cli_core_license_activate");

    // --- spur-pm (5) ---
    pub const PM_CORE_BEADS_BASIC: Self = Self("pm_core_beads_basic");
    pub const PM_CORE_BROWSE: Self = Self("pm_core_browse");
    pub const PM_CORE_PR: Self = Self("pm_core_pr");
    pub const PM_CORE_BEADS_GRAPH_ADAPTER: Self = Self("pm_core_beads_graph_adapter");
    pub const PM_PRO_BEADS_ADVANCED: Self = Self("pm_pro_beads_advanced");

    // --- spur-cost (3) ---
    pub const COST_CORE_SESSION_DISPLAY: Self = Self("cost_core_session_display");
    pub const COST_CORE_PRICING_REGISTRY: Self = Self("cost_core_pricing_registry");
    pub const COST_PRO_PER_PROJECT_TRACKING: Self = Self("cost_pro_per_project_tracking");

    // --- spur-context (1) — Wave 8: consolidated duckdb_engine + daily_report + weekly_report → duckdb_engine ---
    pub const CTX_PRO_DUCKDB_ENGINE: Self = Self("ctx_pro_duckdb_engine");

    // --- spur-worktree (2) ---
    pub const WORKTREE_CORE_ISOLATION: Self = Self("worktree_core_isolation");
    pub const WORKTREE_CORE_ORPHAN_CLEANUP: Self = Self("worktree_core_orphan_cleanup");

    // --- spur-bot (2) — Wave 8: merged thread_registry into telegram_solo ---
    pub const BOT_PRO_TELEGRAM_SOLO: Self = Self("bot_pro_telegram_solo");
    pub const BOT_PRO_INLINE_REVIEW: Self = Self("bot_pro_inline_review");

    // --- spur-license meta (2) ---
    pub const LICENSE_PRO_REVOCATION_POLLING: Self = Self("license_pro_revocation_polling");
    pub const LICENSE_PRO_OFFLINE_GRACE: Self = Self("license_pro_offline_grace");

    // --- spur-blob-store (1: 0 Free + 1 Pro) ---
    pub const BLOB_PRO_NAMESPACE_DELETION: Self = Self("blob_pro_namespace_deletion");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    /// Parse a known feature key from its string representation.
    /// Returns `None` for unknown strings — this is intentional to avoid
    /// shadowing `std::str::FromStr` and to make the "must be in registry"
    /// invariant explicit.
    pub const fn from_known(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if bytes_eq(b, b"acp_core_transport_stdio") {
            Some(Self::ACP_CORE_TRANSPORT_STDIO)
        } else if bytes_eq(b, b"acp_core_transport_socket") {
            Some(Self::ACP_CORE_TRANSPORT_SOCKET)
        } else if bytes_eq(b, b"acp_core_adapter_claude_code") {
            Some(Self::ACP_CORE_ADAPTER_CLAUDE_CODE)
        } else if bytes_eq(b, b"acp_core_adapter_codex") {
            Some(Self::ACP_CORE_ADAPTER_CODEX)
        } else if bytes_eq(b, b"acp_core_adapter_kiro") {
            Some(Self::ACP_CORE_ADAPTER_KIRO)
        } else if bytes_eq(b, b"acp_core_session_attach_advisory_lock") {
            Some(Self::ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK)
        // spur-core: brain
        } else if bytes_eq(b, b"core_core_brain_session") {
            Some(Self::CORE_CORE_BRAIN_SESSION)
        } else if bytes_eq(b, b"core_core_brain_failover_manual_keystroke") {
            Some(Self::CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE)
        // spur-core: workers
        } else if bytes_eq(b, b"core_core_parallel_workers") {
            Some(Self::CORE_CORE_PARALLEL_WORKERS)
        } else if bytes_eq(b, b"core_pro_worker_heartbeat_watchdog") {
            Some(Self::CORE_PRO_WORKER_HEARTBEAT_WATCHDOG)
        // spur-core: event pipeline (Wave 8 NEW umbrella)
        } else if bytes_eq(b, b"core_core_event_pipeline") {
            Some(Self::CORE_CORE_EVENT_PIPELINE)
        // spur-core: peer mailbox
        } else if bytes_eq(b, b"core_pro_peer_mailbox_router") {
            Some(Self::CORE_PRO_PEER_MAILBOX_ROUTER)
        // spur-core: review (Wave 8 umbrella + Wave 9 retry_config tier-shift)
        } else if bytes_eq(b, b"core_core_review") {
            Some(Self::CORE_CORE_REVIEW)
        } else if bytes_eq(b, b"core_core_review_retry_config") {
            Some(Self::CORE_CORE_REVIEW_RETRY_CONFIG)
        } else if bytes_eq(b, b"core_pro_review_auto_approve") {
            Some(Self::CORE_PRO_REVIEW_AUTO_APPROVE)
        // spur-core: system events
        } else if bytes_eq(b, b"core_core_permission_request_detection") {
            Some(Self::CORE_CORE_PERMISSION_REQUEST_DETECTION)
        // spur-core: reliability & lifecycle
        } else if bytes_eq(b, b"core_core_session_resume") {
            Some(Self::CORE_CORE_SESSION_RESUME)
        } else if bytes_eq(b, b"core_pro_session_resume_event_replay") {
            Some(Self::CORE_PRO_SESSION_RESUME_EVENT_REPLAY)
        } else if bytes_eq(b, b"core_core_plan_persistence") {
            Some(Self::CORE_CORE_PLAN_PERSISTENCE)
        // skills
        } else if bytes_eq(b, b"skills_core_registry") {
            Some(Self::SKILLS_CORE_REGISTRY)
        } else if bytes_eq(b, b"skills_pro_custom") {
            Some(Self::SKILLS_PRO_CUSTOM)
        // spur-mcp
        } else if bytes_eq(b, b"mcp_core_server_dispatch") {
            Some(Self::MCP_CORE_SERVER_DISPATCH)
        } else if bytes_eq(b, b"mcp_core_delegate") {
            Some(Self::MCP_CORE_DELEGATE)
        } else if bytes_eq(b, b"mcp_core_outcome_fetch") {
            Some(Self::MCP_CORE_OUTCOME_FETCH)
        } else if bytes_eq(b, b"mcp_core_pm") {
            Some(Self::MCP_CORE_PM)
        } else if bytes_eq(b, b"mcp_core_pr") {
            Some(Self::MCP_CORE_PR)
        } else if bytes_eq(b, b"mcp_core_plan_ephemeral") {
            Some(Self::MCP_CORE_PLAN_EPHEMERAL)
        } else if bytes_eq(b, b"mcp_core_graph_tools") {
            Some(Self::MCP_CORE_GRAPH_TOOLS)
        } else if bytes_eq(b, b"mcp_pro_plan_durable") {
            Some(Self::MCP_PRO_PLAN_DURABLE)
        } else if bytes_eq(b, b"mcp_pro_signal_watcher_scope_drift") {
            Some(Self::MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT)
        } else if bytes_eq(b, b"mcp_pro_review") {
            Some(Self::MCP_PRO_REVIEW)
        // spur-tui
        } else if bytes_eq(b, b"tui_core_view_dashboard") {
            Some(Self::TUI_CORE_VIEW_DASHBOARD)
        } else if bytes_eq(b, b"tui_core_view_session_detail") {
            Some(Self::TUI_CORE_VIEW_SESSION_DETAIL)
        } else if bytes_eq(b, b"tui_core_view_plan_inspector") {
            Some(Self::TUI_CORE_VIEW_PLAN_INSPECTOR)
        } else if bytes_eq(b, b"tui_core_view_palette_overlay") {
            Some(Self::TUI_CORE_VIEW_PALETTE_OVERLAY)
        } else if bytes_eq(b, b"tui_core_view_issue_browser") {
            Some(Self::TUI_CORE_VIEW_ISSUE_BROWSER)
        } else if bytes_eq(b, b"tui_core_modal_collision_escape") {
            Some(Self::TUI_CORE_MODAL_COLLISION_ESCAPE)
        } else if bytes_eq(b, b"tui_core_input_paste_as_atom") {
            Some(Self::TUI_CORE_INPUT_PASTE_AS_ATOM)
        // spur-cli
        } else if bytes_eq(b, b"cli_core_init") {
            Some(Self::CLI_CORE_INIT)
        } else if bytes_eq(b, b"cli_core_agents") {
            Some(Self::CLI_CORE_AGENTS)
        } else if bytes_eq(b, b"cli_core_sessions") {
            Some(Self::CLI_CORE_SESSIONS)
        } else if bytes_eq(b, b"cli_core_run") {
            Some(Self::CLI_CORE_RUN)
        } else if bytes_eq(b, b"cli_core_exec") {
            Some(Self::CLI_CORE_EXEC)
        } else if bytes_eq(b, b"cli_core_tui") {
            Some(Self::CLI_CORE_TUI)
        } else if bytes_eq(b, b"cli_core_cost") {
            Some(Self::CLI_CORE_COST)
        } else if bytes_eq(b, b"cli_core_connect") {
            Some(Self::CLI_CORE_CONNECT)
        } else if bytes_eq(b, b"cli_core_license_activate") {
            Some(Self::CLI_CORE_LICENSE_ACTIVATE)
        // spur-pm
        } else if bytes_eq(b, b"pm_core_beads_basic") {
            Some(Self::PM_CORE_BEADS_BASIC)
        } else if bytes_eq(b, b"pm_core_browse") {
            Some(Self::PM_CORE_BROWSE)
        } else if bytes_eq(b, b"pm_core_pr") {
            Some(Self::PM_CORE_PR)
        } else if bytes_eq(b, b"pm_core_beads_graph_adapter") {
            Some(Self::PM_CORE_BEADS_GRAPH_ADAPTER)
        } else if bytes_eq(b, b"pm_pro_beads_advanced") {
            Some(Self::PM_PRO_BEADS_ADVANCED)
        // spur-cost
        } else if bytes_eq(b, b"cost_core_session_display") {
            Some(Self::COST_CORE_SESSION_DISPLAY)
        } else if bytes_eq(b, b"cost_core_pricing_registry") {
            Some(Self::COST_CORE_PRICING_REGISTRY)
        } else if bytes_eq(b, b"cost_pro_per_project_tracking") {
            Some(Self::COST_PRO_PER_PROJECT_TRACKING)
        // spur-context
        } else if bytes_eq(b, b"ctx_pro_duckdb_engine") {
            Some(Self::CTX_PRO_DUCKDB_ENGINE)
        // spur-worktree
        } else if bytes_eq(b, b"worktree_core_isolation") {
            Some(Self::WORKTREE_CORE_ISOLATION)
        } else if bytes_eq(b, b"worktree_core_orphan_cleanup") {
            Some(Self::WORKTREE_CORE_ORPHAN_CLEANUP)
        // spur-bot
        } else if bytes_eq(b, b"bot_pro_telegram_solo") {
            Some(Self::BOT_PRO_TELEGRAM_SOLO)
        } else if bytes_eq(b, b"bot_pro_inline_review") {
            Some(Self::BOT_PRO_INLINE_REVIEW)
        // spur-license meta
        } else if bytes_eq(b, b"license_pro_revocation_polling") {
            Some(Self::LICENSE_PRO_REVOCATION_POLLING)
        } else if bytes_eq(b, b"license_pro_offline_grace") {
            Some(Self::LICENSE_PRO_OFFLINE_GRACE)
        // spur-blob-store
        } else if bytes_eq(b, b"blob_pro_namespace_deletion") {
            Some(Self::BLOB_PRO_NAMESPACE_DELETION)
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
        assert_eq!(
            FeatureKey::CORE_CORE_BRAIN_SESSION.as_str(),
            "core_core_brain_session"
        );
        assert_eq!(
            FeatureKey::CORE_CORE_PARALLEL_WORKERS.as_str(),
            "core_core_parallel_workers"
        );
        assert_eq!(FeatureKey::PM_CORE_BROWSE.as_str(), "pm_core_browse");
        assert_eq!(
            FeatureKey::BLOB_PRO_NAMESPACE_DELETION.as_str(),
            "blob_pro_namespace_deletion"
        );
    }

    #[test]
    fn copy_eq_and_hash_work() {
        let a = FeatureKey::CORE_CORE_BRAIN_SESSION;
        let b = FeatureKey::CORE_CORE_BRAIN_SESSION;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn from_known_returns_none_for_unknown() {
        assert_eq!(FeatureKey::from_known("not_a_feature"), None);
        assert_eq!(FeatureKey::from_known("kill_advanced_planner"), None);
        assert_eq!(FeatureKey::from_known(""), None);
        assert_eq!(FeatureKey::from_known("Brain_Session"), None); // case-sensitive
    }

    #[test]
    fn unknown_feature_key_display_and_access() {
        let unk = UnknownFeatureKey::new("experimental_thing");
        assert_eq!(unk.as_str(), "experimental_thing");
        assert_eq!(format!("{unk}"), "experimental_thing");
    }

    #[test]
    fn spur_acp_keys_registered() {
        // Wave 8: dropped 3 ghost adapters (cursor/opencode/kimi); Wave 8.5 dropped gemini.
        // Merged degraded_nolock into advisory_lock.
        for s in &[
            "acp_core_transport_stdio",
            "acp_core_transport_socket",
            "acp_core_adapter_claude_code",
            "acp_core_adapter_codex",
            "acp_core_adapter_kiro",
            "acp_core_session_attach_advisory_lock",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        // Wave-8 dropped/absorbed keys must NOT parse anymore.
        for dropped in &[
            "acp_core_adapter_cursor",
            "acp_core_adapter_opencode",
            "acp_core_adapter_kimi",
            "acp_core_adapter_gemini",
            "acp_core_session_attach_degraded_nolock",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8/8.5 dropped key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_brain_keys_registered() {
        // Wave 8: consolidated brain_session+brain_scheduler+continuation_bridge → brain_session.
        // Deferred core_pro_brain_failover_auto_pool to v1.1 backlog (no alternate pool).
        for s in &[
            "core_core_brain_session",
            "core_core_brain_failover_manual_keystroke",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "core_core_brain_scheduler",
            "core_core_continuation_bridge",
            "core_pro_brain_failover_auto_pool",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed/deferred key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_workers_keys_registered() {
        // Wave 8: merged cancellable_semaphore into parallel_workers.
        for s in &[
            "core_core_parallel_workers",
            "core_pro_worker_heartbeat_watchdog",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        assert!(
            FeatureKey::from_known("core_core_cancellable_semaphore").is_none(),
            "Wave-8 absorbed key should not parse"
        );
    }

    #[test]
    fn spur_core_event_pipeline_keys_registered() {
        // Wave 8 NEW umbrella: collapsed funnel+sink+lineage+pump+agent_notification+tui_drain → event_pipeline.
        // Deferred core_pro_broadcast_lagged_recovery to v1.1 (no recovery logic).
        assert!(FeatureKey::from_known("core_core_event_pipeline").is_some());
        for dropped in &[
            "core_core_event_funnel_broadcast",
            "core_core_event_sink_ndjson_128mb",
            "core_core_executor_lineage_projection",
            "core_core_notification_pump",
            "core_core_agent_notification",
            "tui_core_notification_drain",
            "core_pro_broadcast_lagged_recovery",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed/deferred key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_peer_mailbox_keys_registered() {
        // Wave 8: consolidated router+ledger+stranded_recon → router (compile-coupled constructor).
        assert!(FeatureKey::from_known("core_pro_peer_mailbox_router").is_some());
        for dropped in &[
            "core_pro_peer_mailbox_ledger",
            "core_pro_peer_mailbox_stranded_recon",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_review_keys_registered() {
        // Wave 8 NEW umbrella: collapsed sink+timeout+retry → review.
        // Merged core_pro_review_timeout_routing into core_pro_review_auto_approve.
        // Wave 9: tier-shifted retry_config Pro→Free (renamed core_pro→core_core).
        for s in &[
            "core_core_review",
            "core_core_review_retry_config",
            "core_pro_review_auto_approve",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "core_core_review_sink",
            "core_core_review_timeout",
            "core_core_review_retry",
            "core_pro_review_timeout_routing",
            "core_pro_review_retry_config", // Wave 9: renamed to core_core_*
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8/9 absorbed/renamed key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_system_events_keys_registered() {
        // Wave 8: deferred conflict + rate_limit (no production emitters); agent_notification absorbed by event_pipeline.
        assert!(FeatureKey::from_known("core_core_permission_request_detection").is_some());
        for dropped in &[
            "core_core_conflict_detection",
            "core_core_rate_limit_detection",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 deferred key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_core_reliability_keys_registered() {
        // Wave 8: merged plan_orphan_recovery into plan_persistence; dropped background_task_tracker (mechanism plumbing).
        for s in &[
            "core_core_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_plan_persistence",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "core_core_plan_orphan_recovery",
            "core_core_background_task_tracker",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed/dropped key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn skills_keys_registered() {
        // Wave 8: consolidated registry+atomic_installation+render+role_gating → registry.
        for s in &["skills_core_registry", "skills_pro_custom"] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "skills_core_atomic_installation",
            "skills_core_render_per_vendor",
            "skills_pro_role_gating",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_mcp_keys_registered() {
        // Wave 8: merged outcome_materializer→delegate, reconciler_journal_notify→plan_durable,
        // mutation_executor→signal_watcher; deferred custom_tools to v1.1.
        // Wave 9: tier-shifted graph_tools Pro→Free (renamed mcp_pro→mcp_core).
        for s in &[
            "mcp_core_server_dispatch",
            "mcp_core_delegate",
            "mcp_core_outcome_fetch",
            "mcp_core_pm",
            "mcp_core_pr",
            "mcp_core_plan_ephemeral",
            "mcp_core_graph_tools",
            "mcp_pro_plan_durable",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_review",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "mcp_core_outcome_materializer",
            "mcp_pro_reconciler_journal_notify",
            "mcp_pro_mutation_executor",
            "mcp_pro_custom_tools",
            "mcp_pro_graph_tools", // Wave 9: renamed to mcp_core_*
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8/9 absorbed/renamed/deferred key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_tui_keys_registered() {
        // Wave 8: collapsed dashboard+landing+composer → dashboard; notification_drain absorbed by event_pipeline.
        for s in &[
            "tui_core_view_dashboard",
            "tui_core_view_session_detail",
            "tui_core_view_plan_inspector",
            "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser",
            "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        for dropped in &[
            "tui_core_view_landing_decision",
            "tui_core_view_composer",
            "tui_core_notification_drain",
        ] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_cli_keys_registered() {
        for s in &[
            "cli_core_init",
            "cli_core_agents",
            "cli_core_sessions",
            "cli_core_run",
            "cli_core_exec",
            "cli_core_tui",
            "cli_core_cost",
            "cli_core_connect",
            "cli_core_license_activate",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_pm_keys_registered() {
        for s in &[
            "pm_core_beads_basic",
            "pm_core_browse",
            "pm_core_pr",
            "pm_core_beads_graph_adapter",
            "pm_pro_beads_advanced",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_cost_keys_registered() {
        for s in &[
            "cost_core_session_display",
            "cost_core_pricing_registry",
            "cost_pro_per_project_tracking",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_context_keys_registered() {
        // Wave 8: consolidated duckdb_engine + daily_report + weekly_report → duckdb_engine.
        assert!(FeatureKey::from_known("ctx_pro_duckdb_engine").is_some());
        for dropped in &["ctx_pro_daily_report", "ctx_pro_weekly_report"] {
            assert!(
                FeatureKey::from_known(dropped).is_none(),
                "Wave-8 absorbed key {dropped} should not parse"
            );
        }
    }

    #[test]
    fn spur_worktree_keys_registered() {
        for s in &["worktree_core_isolation", "worktree_core_orphan_cleanup"] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_bot_keys_registered() {
        // Wave 8: merged thread_registry into telegram_solo.
        for s in &["bot_pro_telegram_solo", "bot_pro_inline_review"] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
        assert!(
            FeatureKey::from_known("bot_pro_thread_registry").is_none(),
            "Wave-8 absorbed key should not parse"
        );
    }

    #[test]
    fn spur_license_keys_registered() {
        for s in &[
            "license_pro_revocation_polling",
            "license_pro_offline_grace",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_blob_store_keys_registered() {
        assert!(FeatureKey::from_known("blob_pro_namespace_deletion").is_some());
    }

    /// Task 24: Comprehensive roundtrip across every Wave-9-final v1 key.
    /// Total: 63 new v1 keys (47 Free + 15 Pro v1 + 1 Pro v1.1 + 0 Team).
    /// Plan A trajectory: 135 (initial) → 123 (Wave 5) → 107 (Wave 6) → 99 (Wave 7) →
    /// 64 (Wave 8 consolidation) → 64 (Wave 9 tier-shifts; total unchanged) → 63 (Wave 8.5 ghost adapter drop).
    #[test]
    fn tier_revamp_v1_keys_roundtrip() {
        const NEW_KEYS: &[&str] = &[
            // spur-acp (6) — Wave 8 dropped 3 ghost adapters, Wave 8.5 dropped gemini.
            "acp_core_transport_stdio",
            "acp_core_transport_socket",
            "acp_core_adapter_claude_code",
            "acp_core_adapter_codex",
            "acp_core_adapter_kiro",
            "acp_core_session_attach_advisory_lock",
            // spur-core: brain (2) — Wave 8 consolidated trio → brain_session.
            "core_core_brain_session",
            "core_core_brain_failover_manual_keystroke",
            // spur-core: workers (2) — Wave 8 merged cancellable_semaphore.
            "core_core_parallel_workers",
            "core_pro_worker_heartbeat_watchdog",
            // spur-core: event pipeline (1) — Wave 8 NEW umbrella.
            "core_core_event_pipeline",
            // spur-core: review (3) — Wave 8 umbrella + Wave 9 retry_config tier-shift.
            "core_core_review",
            "core_core_review_retry_config",
            "core_pro_review_auto_approve",
            // skills (2) — Wave 8 quartet → registry.
            "skills_core_registry",
            "skills_pro_custom",
            // spur-core: peer mailbox (1) — Wave 8 trio → router.
            "core_pro_peer_mailbox_router",
            // spur-core: system events (1).
            "core_core_permission_request_detection",
            // spur-core: reliability & lifecycle (3).
            "core_core_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_plan_persistence",
            // spur-mcp (10) — Wave 8 merges + Wave 9 graph_tools tier-shift.
            "mcp_core_server_dispatch",
            "mcp_core_delegate",
            "mcp_core_outcome_fetch",
            "mcp_core_pm",
            "mcp_core_pr",
            "mcp_core_plan_ephemeral",
            "mcp_core_graph_tools",
            "mcp_pro_plan_durable",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_review",
            // spur-tui (7) — Wave 8 collapsed dashboard trio + drain absorbed.
            "tui_core_view_dashboard",
            "tui_core_view_session_detail",
            "tui_core_view_plan_inspector",
            "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser",
            "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom",
            // spur-cli (9) — KEEP_ATOMIC.
            "cli_core_init",
            "cli_core_agents",
            "cli_core_sessions",
            "cli_core_run",
            "cli_core_exec",
            "cli_core_tui",
            "cli_core_cost",
            "cli_core_connect",
            "cli_core_license_activate",
            // spur-pm (5) — KEEP_ATOMIC with prereq.
            "pm_core_beads_basic",
            "pm_core_browse",
            "pm_core_pr",
            "pm_core_beads_graph_adapter",
            "pm_pro_beads_advanced",
            // spur-cost (3) — KEEP_ATOMIC with pricing_registry prereq.
            "cost_core_session_display",
            "cost_core_pricing_registry",
            "cost_pro_per_project_tracking",
            // spur-context (1) — Wave 8 absorbed daily/weekly reports.
            "ctx_pro_duckdb_engine",
            // spur-worktree (2) — KEEP_ATOMIC with prereq.
            "worktree_core_isolation",
            "worktree_core_orphan_cleanup",
            // spur-bot (2) — Wave 8 merged thread_registry.
            "bot_pro_telegram_solo",
            "bot_pro_inline_review",
            // spur-license meta (2) — KEEP_ATOMIC with prereq.
            "license_pro_revocation_polling",
            "license_pro_offline_grace",
            // spur-blob-store (1) — Wave 7 final.
            "blob_pro_namespace_deletion",
        ];

        assert_eq!(
            NEW_KEYS.len(),
            63,
            "Expected exactly 63 new tier-revamp v1 keys post-Wave-8.5; got {}. \
             Trajectory: 135 -> 123 (W5) -> 107 (W6) -> 99 (W7) -> 64 (W8 consolidation) -> 64 (W9 tier-shifts) -> 63 (W8.5 ghost adapter drop).",
            NEW_KEYS.len()
        );

        let mut seen = std::collections::HashSet::new();
        for s in NEW_KEYS {
            let parsed = FeatureKey::from_known(s);
            assert!(parsed.is_some(), "key {s:?} not parseable via from_known");
            let key = parsed.unwrap();
            assert_eq!(key.as_str(), *s, "as_str roundtrip mismatch for {s}");
            assert!(seen.insert(*s), "duplicate key in test list: {s}");
        }
    }
}
