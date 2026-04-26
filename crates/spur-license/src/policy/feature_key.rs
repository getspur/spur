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

    // --- spur-core: brain & scheduling (5) ---
    pub const CORE_CORE_BRAIN_SESSION: Self = Self("core_core_brain_session");
    pub const CORE_CORE_BRAIN_SCHEDULER: Self = Self("core_core_brain_scheduler");
    pub const CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE: Self = Self("core_core_brain_failover_manual_keystroke");
    pub const CORE_PRO_BRAIN_FAILOVER_AUTO_POOL: Self = Self("core_pro_brain_failover_auto_pool");
    pub const CORE_CORE_CONTINUATION_BRIDGE: Self = Self("core_core_continuation_bridge");

    // --- spur-core: workers & semaphore (3) ---
    pub const CORE_CORE_PARALLEL_WORKERS: Self = Self("core_core_parallel_workers");
    pub const CORE_CORE_CANCELLABLE_SEMAPHORE: Self = Self("core_core_cancellable_semaphore");
    pub const CORE_PRO_WORKER_HEARTBEAT_WATCHDOG: Self = Self("core_pro_worker_heartbeat_watchdog");

    // --- spur-core: event pipeline (5) ---
    pub const CORE_CORE_EVENT_FUNNEL_BROADCAST: Self = Self("core_core_event_funnel_broadcast");
    pub const CORE_CORE_EVENT_SINK_NDJSON_128MB: Self = Self("core_core_event_sink_ndjson_128mb");
    pub const CORE_CORE_EXECUTOR_LINEAGE_PROJECTION: Self =
        Self("core_core_executor_lineage_projection");
    pub const CORE_CORE_NOTIFICATION_PUMP: Self = Self("core_core_notification_pump");
    pub const CORE_PRO_BROADCAST_LAGGED_RECOVERY: Self = Self("core_pro_broadcast_lagged_recovery");

    // --- spur-core: peer mailbox (3) ---
    pub const CORE_PRO_PEER_MAILBOX_ROUTER: Self = Self("core_pro_peer_mailbox_router");
    pub const CORE_PRO_PEER_MAILBOX_LEDGER: Self = Self("core_pro_peer_mailbox_ledger");
    pub const CORE_PRO_PEER_MAILBOX_STRANDED_RECON: Self =
        Self("core_pro_peer_mailbox_stranded_recon");

    // --- spur-core: review subsystem (6) ---
    pub const CORE_CORE_REVIEW_SINK: Self = Self("core_core_review_sink");
    pub const CORE_CORE_REVIEW_TIMEOUT: Self = Self("core_core_review_timeout");
    pub const CORE_CORE_REVIEW_RETRY: Self = Self("core_core_review_retry");
    pub const CORE_PRO_REVIEW_AUTO_APPROVE: Self = Self("core_pro_review_auto_approve");
    pub const CORE_PRO_REVIEW_TIMEOUT_ROUTING: Self = Self("core_pro_review_timeout_routing");
    pub const CORE_PRO_REVIEW_RETRY_CONFIG: Self = Self("core_pro_review_retry_config");

    // --- spur-core: system events (4) ---
    pub const CORE_CORE_CONFLICT_DETECTION: Self = Self("core_core_conflict_detection");
    pub const CORE_CORE_RATE_LIMIT_DETECTION: Self = Self("core_core_rate_limit_detection");
    pub const CORE_CORE_PERMISSION_REQUEST_DETECTION: Self =
        Self("core_core_permission_request_detection");
    pub const CORE_CORE_AGENT_NOTIFICATION: Self = Self("core_core_agent_notification");

    // --- spur-core: reliability & lifecycle (5) ---
    pub const CORE_CORE_SESSION_RESUME: Self = Self("core_core_session_resume");
    pub const CORE_PRO_SESSION_RESUME_EVENT_REPLAY: Self =
        Self("core_pro_session_resume_event_replay");
    pub const CORE_CORE_PLAN_PERSISTENCE: Self = Self("core_core_plan_persistence");
    pub const CORE_CORE_PLAN_ORPHAN_RECOVERY: Self = Self("core_core_plan_orphan_recovery");
    pub const CORE_CORE_BACKGROUND_TASK_TRACKER: Self =
        Self("core_core_background_task_tracker");

    // --- skills (5) ---
    pub const SKILLS_CORE_REGISTRY: Self = Self("skills_core_registry");
    pub const SKILLS_CORE_ATOMIC_INSTALLATION: Self = Self("skills_core_atomic_installation");
    pub const SKILLS_CORE_RENDER_PER_VENDOR: Self = Self("skills_core_render_per_vendor");
    pub const SKILLS_PRO_CUSTOM: Self = Self("skills_pro_custom");
    pub const SKILLS_PRO_ROLE_GATING: Self = Self("skills_pro_role_gating");

    // --- spur-mcp (14) ---
    pub const MCP_CORE_SERVER_DISPATCH: Self = Self("mcp_core_server_dispatch");
    pub const MCP_CORE_DELEGATE: Self = Self("mcp_core_delegate");
    pub const MCP_CORE_OUTCOME_FETCH: Self = Self("mcp_core_outcome_fetch");
    pub const MCP_CORE_PM: Self = Self("mcp_core_pm");
    pub const MCP_CORE_PR: Self = Self("mcp_core_pr");
    pub const MCP_CORE_PLAN_EPHEMERAL: Self = Self("mcp_core_plan_ephemeral");
    pub const MCP_CORE_OUTCOME_MATERIALIZER: Self = Self("mcp_core_outcome_materializer");
    pub const MCP_PRO_PLAN_DURABLE: Self = Self("mcp_pro_plan_durable");
    pub const MCP_PRO_RECONCILER_JOURNAL_NOTIFY: Self = Self("mcp_pro_reconciler_journal_notify");
    pub const MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT: Self = Self("mcp_pro_signal_watcher_scope_drift");
    pub const MCP_PRO_MUTATION_EXECUTOR: Self = Self("mcp_pro_mutation_executor");
    pub const MCP_PRO_GRAPH_TOOLS: Self = Self("mcp_pro_graph_tools");
    pub const MCP_PRO_REVIEW: Self = Self("mcp_pro_review");
    pub const MCP_PRO_CUSTOM_TOOLS: Self = Self("mcp_pro_custom_tools");

    // --- spur-tui (10) ---
    pub const TUI_CORE_VIEW_DASHBOARD: Self = Self("tui_core_view_dashboard");
    pub const TUI_CORE_VIEW_SESSION_DETAIL: Self = Self("tui_core_view_session_detail");
    pub const TUI_CORE_VIEW_PLAN_INSPECTOR: Self = Self("tui_core_view_plan_inspector");
    pub const TUI_CORE_VIEW_PALETTE_OVERLAY: Self = Self("tui_core_view_palette_overlay");
    pub const TUI_CORE_VIEW_ISSUE_BROWSER: Self = Self("tui_core_view_issue_browser");
    pub const TUI_CORE_VIEW_LANDING_DECISION: Self = Self("tui_core_view_landing_decision");
    pub const TUI_CORE_VIEW_COMPOSER: Self = Self("tui_core_view_composer");
    pub const TUI_CORE_MODAL_COLLISION_ESCAPE: Self = Self("tui_core_modal_collision_escape");
    pub const TUI_CORE_INPUT_PASTE_AS_ATOM: Self = Self("tui_core_input_paste_as_atom");
    pub const TUI_CORE_NOTIFICATION_DRAIN: Self = Self("tui_core_notification_drain");

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

    // --- spur-worktree (2) ---
    pub const WORKTREE_CORE_ISOLATION: Self = Self("worktree_core_isolation");
    pub const WORKTREE_CORE_ORPHAN_CLEANUP: Self = Self("worktree_core_orphan_cleanup");

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
        // spur-core: brain & scheduling
        } else if bytes_eq(b, b"core_core_brain_session") {
            Some(Self::CORE_CORE_BRAIN_SESSION)
        } else if bytes_eq(b, b"core_core_brain_scheduler") {
            Some(Self::CORE_CORE_BRAIN_SCHEDULER)
        } else if bytes_eq(b, b"core_core_brain_failover_manual_keystroke") {
            Some(Self::CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE)
        } else if bytes_eq(b, b"core_pro_brain_failover_auto_pool") {
            Some(Self::CORE_PRO_BRAIN_FAILOVER_AUTO_POOL)
        } else if bytes_eq(b, b"core_core_continuation_bridge") {
            Some(Self::CORE_CORE_CONTINUATION_BRIDGE)
        // spur-core: workers & semaphore
        } else if bytes_eq(b, b"core_core_parallel_workers") {
            Some(Self::CORE_CORE_PARALLEL_WORKERS)
        } else if bytes_eq(b, b"core_core_cancellable_semaphore") {
            Some(Self::CORE_CORE_CANCELLABLE_SEMAPHORE)
        } else if bytes_eq(b, b"core_pro_worker_heartbeat_watchdog") {
            Some(Self::CORE_PRO_WORKER_HEARTBEAT_WATCHDOG)
        // spur-core: event pipeline
        } else if bytes_eq(b, b"core_core_event_funnel_broadcast") {
            Some(Self::CORE_CORE_EVENT_FUNNEL_BROADCAST)
        } else if bytes_eq(b, b"core_core_event_sink_ndjson_128mb") {
            Some(Self::CORE_CORE_EVENT_SINK_NDJSON_128MB)
        } else if bytes_eq(b, b"core_core_executor_lineage_projection") {
            Some(Self::CORE_CORE_EXECUTOR_LINEAGE_PROJECTION)
        } else if bytes_eq(b, b"core_core_notification_pump") {
            Some(Self::CORE_CORE_NOTIFICATION_PUMP)
        } else if bytes_eq(b, b"core_pro_broadcast_lagged_recovery") {
            Some(Self::CORE_PRO_BROADCAST_LAGGED_RECOVERY)
        // spur-core: peer mailbox
        } else if bytes_eq(b, b"core_pro_peer_mailbox_router") {
            Some(Self::CORE_PRO_PEER_MAILBOX_ROUTER)
        } else if bytes_eq(b, b"core_pro_peer_mailbox_ledger") {
            Some(Self::CORE_PRO_PEER_MAILBOX_LEDGER)
        } else if bytes_eq(b, b"core_pro_peer_mailbox_stranded_recon") {
            Some(Self::CORE_PRO_PEER_MAILBOX_STRANDED_RECON)
        // spur-core: review subsystem
        } else if bytes_eq(b, b"core_core_review_sink") {
            Some(Self::CORE_CORE_REVIEW_SINK)
        } else if bytes_eq(b, b"core_core_review_timeout") {
            Some(Self::CORE_CORE_REVIEW_TIMEOUT)
        } else if bytes_eq(b, b"core_core_review_retry") {
            Some(Self::CORE_CORE_REVIEW_RETRY)
        } else if bytes_eq(b, b"core_pro_review_auto_approve") {
            Some(Self::CORE_PRO_REVIEW_AUTO_APPROVE)
        } else if bytes_eq(b, b"core_pro_review_timeout_routing") {
            Some(Self::CORE_PRO_REVIEW_TIMEOUT_ROUTING)
        } else if bytes_eq(b, b"core_pro_review_retry_config") {
            Some(Self::CORE_PRO_REVIEW_RETRY_CONFIG)
        // spur-core: system events
        } else if bytes_eq(b, b"core_core_conflict_detection") {
            Some(Self::CORE_CORE_CONFLICT_DETECTION)
        } else if bytes_eq(b, b"core_core_rate_limit_detection") {
            Some(Self::CORE_CORE_RATE_LIMIT_DETECTION)
        } else if bytes_eq(b, b"core_core_permission_request_detection") {
            Some(Self::CORE_CORE_PERMISSION_REQUEST_DETECTION)
        } else if bytes_eq(b, b"core_core_agent_notification") {
            Some(Self::CORE_CORE_AGENT_NOTIFICATION)
        // spur-core: reliability & lifecycle
        } else if bytes_eq(b, b"core_core_session_resume") {
            Some(Self::CORE_CORE_SESSION_RESUME)
        } else if bytes_eq(b, b"core_pro_session_resume_event_replay") {
            Some(Self::CORE_PRO_SESSION_RESUME_EVENT_REPLAY)
        } else if bytes_eq(b, b"core_core_plan_persistence") {
            Some(Self::CORE_CORE_PLAN_PERSISTENCE)
        } else if bytes_eq(b, b"core_core_plan_orphan_recovery") {
            Some(Self::CORE_CORE_PLAN_ORPHAN_RECOVERY)
        } else if bytes_eq(b, b"core_core_background_task_tracker") {
            Some(Self::CORE_CORE_BACKGROUND_TASK_TRACKER)
        // skills
        } else if bytes_eq(b, b"skills_core_registry") {
            Some(Self::SKILLS_CORE_REGISTRY)
        } else if bytes_eq(b, b"skills_core_atomic_installation") {
            Some(Self::SKILLS_CORE_ATOMIC_INSTALLATION)
        } else if bytes_eq(b, b"skills_core_render_per_vendor") {
            Some(Self::SKILLS_CORE_RENDER_PER_VENDOR)
        } else if bytes_eq(b, b"skills_pro_custom") {
            Some(Self::SKILLS_PRO_CUSTOM)
        } else if bytes_eq(b, b"skills_pro_role_gating") {
            Some(Self::SKILLS_PRO_ROLE_GATING)
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
        } else if bytes_eq(b, b"mcp_core_outcome_materializer") {
            Some(Self::MCP_CORE_OUTCOME_MATERIALIZER)
        } else if bytes_eq(b, b"mcp_pro_plan_durable") {
            Some(Self::MCP_PRO_PLAN_DURABLE)
        } else if bytes_eq(b, b"mcp_pro_reconciler_journal_notify") {
            Some(Self::MCP_PRO_RECONCILER_JOURNAL_NOTIFY)
        } else if bytes_eq(b, b"mcp_pro_signal_watcher_scope_drift") {
            Some(Self::MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT)
        } else if bytes_eq(b, b"mcp_pro_mutation_executor") {
            Some(Self::MCP_PRO_MUTATION_EXECUTOR)
        } else if bytes_eq(b, b"mcp_pro_graph_tools") {
            Some(Self::MCP_PRO_GRAPH_TOOLS)
        } else if bytes_eq(b, b"mcp_pro_review") {
            Some(Self::MCP_PRO_REVIEW)
        } else if bytes_eq(b, b"mcp_pro_custom_tools") {
            Some(Self::MCP_PRO_CUSTOM_TOOLS)
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
        } else if bytes_eq(b, b"tui_core_view_landing_decision") {
            Some(Self::TUI_CORE_VIEW_LANDING_DECISION)
        } else if bytes_eq(b, b"tui_core_view_composer") {
            Some(Self::TUI_CORE_VIEW_COMPOSER)
        } else if bytes_eq(b, b"tui_core_modal_collision_escape") {
            Some(Self::TUI_CORE_MODAL_COLLISION_ESCAPE)
        } else if bytes_eq(b, b"tui_core_input_paste_as_atom") {
            Some(Self::TUI_CORE_INPUT_PASTE_AS_ATOM)
        } else if bytes_eq(b, b"tui_core_notification_drain") {
            Some(Self::TUI_CORE_NOTIFICATION_DRAIN)
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
        // spur-worktree
        } else if bytes_eq(b, b"worktree_core_isolation") {
            Some(Self::WORKTREE_CORE_ISOLATION)
        } else if bytes_eq(b, b"worktree_core_orphan_cleanup") {
            Some(Self::WORKTREE_CORE_ORPHAN_CLEANUP)
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

    #[test]
    fn spur_core_brain_keys_registered() {
        for s in &[
            "core_core_brain_session",
            "core_core_brain_scheduler",
            "core_core_brain_failover_manual_keystroke",
            "core_pro_brain_failover_auto_pool",
            "core_core_continuation_bridge",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_workers_keys_registered() {
        for s in &[
            "core_core_parallel_workers",
            "core_core_cancellable_semaphore",
            "core_pro_worker_heartbeat_watchdog",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_event_pipeline_keys_registered() {
        for s in &[
            "core_core_event_funnel_broadcast",
            "core_core_event_sink_ndjson_128mb",
            "core_core_executor_lineage_projection",
            "core_core_notification_pump",
            "core_pro_broadcast_lagged_recovery",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_peer_mailbox_keys_registered() {
        for s in &[
            "core_pro_peer_mailbox_router",
            "core_pro_peer_mailbox_ledger",
            "core_pro_peer_mailbox_stranded_recon",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_review_keys_registered() {
        for s in &[
            "core_core_review_sink",
            "core_core_review_timeout",
            "core_core_review_retry",
            "core_pro_review_auto_approve",
            "core_pro_review_timeout_routing",
            "core_pro_review_retry_config",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_system_events_keys_registered() {
        for s in &[
            "core_core_conflict_detection",
            "core_core_rate_limit_detection",
            "core_core_permission_request_detection",
            "core_core_agent_notification",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_core_reliability_keys_registered() {
        for s in &[
            "core_core_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_plan_persistence",
            "core_core_plan_orphan_recovery",
            "core_core_background_task_tracker",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn skills_keys_registered() {
        for s in &[
            "skills_core_registry",
            "skills_core_atomic_installation",
            "skills_core_render_per_vendor",
            "skills_pro_custom",
            "skills_pro_role_gating",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_mcp_keys_registered() {
        for s in &[
            "mcp_core_server_dispatch",
            "mcp_core_delegate",
            "mcp_core_outcome_fetch",
            "mcp_core_pm",
            "mcp_core_pr",
            "mcp_core_plan_ephemeral",
            "mcp_core_outcome_materializer",
            "mcp_pro_plan_durable",
            "mcp_pro_reconciler_journal_notify",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_mutation_executor",
            "mcp_pro_graph_tools",
            "mcp_pro_review",
            "mcp_pro_custom_tools",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }

    #[test]
    fn spur_tui_keys_registered() {
        for s in &[
            "tui_core_view_dashboard",
            "tui_core_view_session_detail",
            "tui_core_view_plan_inspector",
            "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser",
            "tui_core_view_landing_decision",
            "tui_core_view_composer",
            "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom",
            "tui_core_notification_drain",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
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
    fn spur_worktree_keys_registered() {
        for s in &[
            "worktree_core_isolation",
            "worktree_core_orphan_cleanup",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
}
