pub mod entries;
pub mod hooks;
pub mod validator;

pub use entries::{
    CommandsConfig, DisplayConfig, IngestBinding, PermissionsConfig, ResponseBinding,
    StaticCommandDecl,
};
pub use hooks::{
    ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind, ResponseRenderKind,
};
pub use validator::{validate_agent_config, ConfigError};

use crate::domain::delegation::TimeoutFallback;
use crate::types::{AgentKind, AgentRole, CostTier, TransportKind};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Per-agent task-routing descriptor. Feeds both the brain prompt and
/// `list_available_workers` tool response. See design spec section A.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DelegationDescriptor {
    /// One-line human summary. Rendered into the workers-block of the
    /// brain prompt when non-empty.
    pub description: Option<String>,
    /// Routing preference signal. Specialists are preferred when their
    /// good_for matches; generalists are fallback.
    pub tier: Option<Tier>,
    /// Positive task patterns. Used by the brain to route.
    pub good_for: Vec<String>,
    /// Negative task patterns. Soft signal; brain MAY override with
    /// stated rationale when no better agent exists.
    pub avoid_for: Vec<String>,
    /// Held back from workers-block; injected into per-dispatch task
    /// prompt only.
    pub strengths: Vec<String>,
    /// Held back from workers-block; injected into per-dispatch task
    /// prompt only.
    pub limitations: Vec<String>,
    /// Held back from routing; shown to brain when dispatching so it
    /// can shape CONTEXT appropriately.
    pub input_expectations: Option<String>,
    /// Routing-relevant via `list_available_workers`. Brain uses for
    /// EXPECTED_OUTPUT section of dispatched task prompt.
    pub output_shape: Option<String>,
    /// Default true. When false, user fields are used verbatim
    /// (including empty vecs — no built-in merge).
    // Field-level default needed even with struct-level `#[serde(default)]`:
    // the struct-level default only fires when the whole block is absent.
    #[serde(default = "default_true")]
    pub inherit_defaults: bool,
}

impl Default for DelegationDescriptor {
    fn default() -> Self {
        Self {
            description: None,
            tier: None,
            good_for: Vec::new(),
            avoid_for: Vec::new(),
            strengths: Vec::new(),
            limitations: Vec::new(),
            input_expectations: None,
            output_shape: None,
            inherit_defaults: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Specialist,
    Generalist,
}

/// Configuration for a single registered agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique name for this agent (e.g., "kiro", "claude-code").
    pub name: String,
    /// Command to execute (e.g., "kiro-cli", "claude").
    pub command: String,
    /// Arguments to pass to the command (e.g., ["acp"], ["--experimental-acp"]).
    #[serde(default)]
    pub args: Vec<String>,
    /// Which transport protocol to use.
    pub transport: TransportKind,
    /// Wire-level idiom used by the adapter layer for TUI rendering.
    /// Orthogonal to `transport`: multiple kinds share the same transport.
    /// Defaults to `Generic` for unknown agents (safe heuristic fallback).
    #[serde(default)]
    pub kind: AgentKind,
    /// Whether this agent can be a brain, worker, or both.
    #[serde(default = "default_role")]
    pub role: AgentRole,
    /// Capability tags for routing (e.g., ["security", "tests", "python"]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Cost tier for estimation.
    #[serde(default = "default_cost_tier")]
    pub cost_tier: CostTier,
    /// Rate limit window duration, if known.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_duration_serde"
    )]
    pub rate_limit_window: Option<Duration>,
    /// Human-review policy for delegations to this agent.
    #[serde(default)]
    pub review: AgentReviewPolicy,

    /// Per-agent display metadata (short handle, display name). Optional;
    /// defaults applied by `effective_handle`.
    #[serde(default)]
    pub display: DisplayConfig,

    /// Command dispatch / vendor-ext wiring. Optional; defaults to
    /// `prompt_text` dispatch with no vendor-ext ingest or response.
    #[serde(default)]
    pub commands: CommandsConfig,

    /// Permission-bypass levers. Replaces the three flat `skip_permissions*`
    /// fields; those remain for backward compatibility and are consulted by
    /// `effective_permissions` when this block is left at default.
    #[serde(default)]
    pub permissions: PermissionsConfig,

    /// Enables bypass mode for this agent. When true, `skip_permissions_args`
    /// (if any) are appended at spawn, `skip_permissions_session_mode` (if
    /// set) is applied after session creation, and any ACP permission
    /// requests are auto-approved. See
    /// `docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md`
    /// for the full mechanism. Default: false.
    #[serde(default)]
    pub skip_permissions: bool,

    /// Spawn-time CLI args appended to `args` when `skip_permissions = true`.
    /// Use for agents whose bypass is a command-line flag
    /// (e.g. `["--trust-all-tools"]` for kiro-cli,
    /// `["--dangerously-skip-permissions"]` for claude direct).
    /// Default: empty.
    #[serde(default)]
    pub skip_permissions_args: Vec<String>,

    /// ACP session mode to set via `set_session_mode` right after
    /// `new_session`, when `skip_permissions = true`. Use for agents that
    /// expose bypass as an ACP session mode (claude-code-acp →
    /// `"bypassPermissions"`). Non-fatal if the agent rejects the mode:
    /// L2 auto-approve still catches any permission calls.
    /// Default: None.
    #[serde(default)]
    pub skip_permissions_session_mode: Option<String>,

    /// Task-routing descriptor for delegation decisions. See
    /// `docs/spur/agent-config.md` and the delegation-framework spec.
    #[serde(default)]
    pub delegation: DelegationDescriptor,
}

impl AgentConfig {
    /// Construct an AgentConfig with all-default sub-tables and empty
    /// command/args, identified by `name`. Used by the TUI's fallback
    /// path (agent referenced but not listed in `.spur/config.toml`)
    /// and by test fixtures that need a minimal config stub.
    pub fn with_defaults(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: String::new(),
            args: Vec::new(),
            transport: crate::types::TransportKind::Acp,
            kind: crate::types::AgentKind::Generic,
            role: crate::types::AgentRole::Both,
            capabilities: Vec::new(),
            cost_tier: crate::types::CostTier::Medium,
            rate_limit_window: None,
            review: AgentReviewPolicy::default(),
            display: DisplayConfig::default(),
            commands: CommandsConfig::default(),
            permissions: PermissionsConfig::default(),
            skip_permissions: false,
            skip_permissions_args: Vec::new(),
            skip_permissions_session_mode: None,
            delegation: DelegationDescriptor::default(),
        }
    }

    /// The effective permissions for this agent, merging the legacy flat
    /// `skip_permissions*` fields with the newer `[permissions]` nested
    /// block. Precedence: if the nested block has ANY non-default value
    /// (`skip`, `args`, or `session_mode`), it wins entirely. Otherwise the
    /// flat fields are promoted into a `PermissionsConfig`.
    ///
    /// The flat fields are retained for one release cycle for back-compat.
    /// New configs should write the nested form.
    pub fn effective_permissions(&self) -> PermissionsConfig {
        let nested_is_default = !self.permissions.skip
            && self.permissions.args.is_empty()
            && self.permissions.session_mode.is_none();
        if nested_is_default {
            PermissionsConfig {
                skip: self.skip_permissions,
                args: self.skip_permissions_args.clone(),
                session_mode: self.skip_permissions_session_mode.clone(),
            }
        } else {
            self.permissions.clone()
        }
    }

    /// The short handle used as `/handle:cmd` on collision and as the
    /// key under which an agent's commands register. Prefers
    /// `display.handle` when set, otherwise falls back to
    /// `name.to_lowercase()`.
    pub fn effective_handle(&self) -> String {
        self.display
            .handle
            .clone()
            .unwrap_or_else(|| self.name.to_lowercase())
    }

    /// Args to pass when spawning this agent. Concatenates `args` with the
    /// effective `permissions.args` iff `permissions.skip` is true. Single
    /// source of truth for `spur-core`'s spawn paths — do not read
    /// `self.args` directly when spawning.
    pub fn effective_args(&self) -> Vec<String> {
        let mut out = self.args.clone();
        let perms = self.effective_permissions();
        if perms.skip {
            out.extend(perms.args.iter().cloned());
        }
        out
    }
}

fn default_role() -> AgentRole {
    AgentRole::Worker
}

fn default_cost_tier() -> CostTier {
    CostTier::Medium
}

/// Per-agent human-review policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReviewPolicy {
    /// When true, the orchestrator gates every delegation to this agent
    /// on a human review. Default: false.
    #[serde(default)]
    pub review_required: bool,
    /// How long to wait for a human decision before applying the default.
    /// Serialized as `review_timeout_secs` (integer seconds) in TOML.
    #[serde(
        default = "default_review_timeout",
        rename = "review_timeout_secs",
        with = "duration_secs_serde"
    )]
    pub review_timeout: Duration,
    /// What to apply on timeout.
    /// Default: `TimeoutFallback::Reject { reason: "review timeout" }`.
    #[serde(default = "default_review_timeout_default")]
    pub review_timeout_default: TimeoutFallback,
    /// Cap on `ReviewDecision::Retry` loops. Default: 3.
    #[serde(default = "default_max_review_retries")]
    pub max_review_retries: u32,
}

impl Default for AgentReviewPolicy {
    fn default() -> Self {
        Self {
            review_required: false,
            review_timeout: default_review_timeout(),
            review_timeout_default: default_review_timeout_default(),
            max_review_retries: default_max_review_retries(),
        }
    }
}

fn default_review_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}

fn default_review_timeout_default() -> TimeoutFallback {
    TimeoutFallback::Reject {
        reason: "review timeout".into(),
    }
}

fn default_max_review_retries() -> u32 {
    3
}

mod duration_secs_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

/// Runtime-level SPUR configuration knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpurRuntimeConfig {
    /// When true, automatically merge approved plans and create PRs.
    /// Default: false (opt-in).
    pub auto_merge_approved_plans: bool,
    /// Grace period before startup quarantines stale `spur:plan-pending`
    /// persisted plan epics. Default: 1 hour.
    pub plan_pending_grace_secs: u64,
    /// Duration of an in-flight dispatch lease. Default: 10 minutes.
    pub dispatch_lease_secs: u64,
    /// Cadence used by the orchestrator to renew dispatch leases.
    /// Default: 200 seconds (one third of the default lease).
    pub dispatch_lease_heartbeat_secs: u64,
}

fn default_plan_pending_grace_secs() -> u64 {
    60 * 60
}

fn default_dispatch_lease_secs() -> u64 {
    600
}

fn default_dispatch_lease_heartbeat_secs() -> u64 {
    default_dispatch_lease_secs() / 3
}

impl Default for SpurRuntimeConfig {
    fn default() -> Self {
        Self {
            auto_merge_approved_plans: false,
            plan_pending_grace_secs: default_plan_pending_grace_secs(),
            dispatch_lease_secs: default_dispatch_lease_secs(),
            dispatch_lease_heartbeat_secs: default_dispatch_lease_heartbeat_secs(),
        }
    }
}

/// Global SPUR configuration (from ~/.spur/config.toml + .spur/config.toml).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BotConfig {
    pub telegram: TelegramBotConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramBotConfig {
    pub enabled: bool,
    pub bot_token: Option<String>,
    pub operator_user_id: Option<i64>,
    pub poll_timeout_secs: u64,
    pub request_timeout_secs: Option<u64>,
    pub draft_streaming: bool,
    pub max_requests_per_second: u32,
}

impl Default for TelegramBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: None,
            operator_user_id: None,
            poll_timeout_secs: 30,
            request_timeout_secs: None,
            draft_streaming: false,
            max_requests_per_second: 20,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpurConfig {
    #[serde(default)]
    pub brain: BrainConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub failover: FailoverConfig,
    #[serde(default)]
    pub worktree: WorktreeConfig,
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub pm: PmConfig,
    #[serde(default)]
    pub bot: BotConfig,
    #[serde(default)]
    pub project: Option<ProjectConfig>,
    /// Delegation dispatch tuning (Phase 1c async-first migration).
    #[serde(default)]
    pub delegation: DelegationConfig,
    /// Runtime SPUR knobs (v0e+).
    #[serde(default)]
    pub spur: SpurRuntimeConfig,
    /// Stage-1 worker peer mailbox feature flag. Default off until explicitly
    /// opted in by production callers.
    #[serde(default)]
    pub peer_mailbox_enabled: bool,
    /// TUI presentation preferences (edit mode today; mouse/density/keymap
    /// in the future). Skipped on serialize when default to keep existing
    /// configs visually unchanged.
    #[serde(default, skip_serializing_if = "TuiConfig::is_default")]
    pub tui: TuiConfig,
    #[serde(default)]
    pub log: LogConfig,
}

/// Runtime knobs for `delegate_to_worker` / `delegate_parallel` dispatch.
///
/// Default is **pure async-first** (`inline_wait_ms = 0`): every delegation
/// falls through to the detached path and returns via the continuation
/// bridge, never via `completed_delegations` polling. A non-zero value gives
/// the worker a short inline window to complete before handing the receiver
/// to the background collector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    /// How long the MCP handler will wait inline for a worker to respond
    /// before handing the oneshot receiver to the detached collector and
    /// returning `status: "pending"` with `continuation_will_fire: true`.
    /// Default `0` — async-first.
    pub inline_wait_ms: u64,
    /// Orchestrator-side worker branch normalization settings.
    pub normalize: DelegationNormalizeConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationNormalizeConfig {
    /// When true, branch normalization git commits use `--no-verify` and
    /// `--no-gpg-sign`. Default false so user repository hooks and signing
    /// policies remain authoritative.
    pub bypass_hooks: bool,
}

/// Editing-mode preference for the TUI input bar. Stored in config so the
/// choice survives `spur tui` restarts. Maps to runtime `spur_tui::EditMode`
/// via `From` impl in `spur-tui`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    /// Emacs-style keybindings (default; uses tui-textarea built-ins).
    #[default]
    Emacs,
    /// Vim modal editing.
    Vim,
}

/// TUI presentation preferences. New fields land here without schema churn.
/// Future additions (mouse, density, keymap) extend this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub edit_mode: EditorMode,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disable_paste_burst: bool,
    /// Active theme name. Resolved at startup via `.spur/themes/<name>.yaml`
    /// (project) → `~/.spur/themes/<name>.yaml` (user) → built-in. Defaults
    /// to `"dark"` for backwards compatibility — the dark built-in is a
    /// pixel-perfect reproduction of pre-theme TUI colors.
    #[serde(default = "default_theme_name", skip_serializing_if = "is_dark_theme")]
    pub theme: String,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            edit_mode: EditorMode::default(),
            disable_paste_burst: false,
            theme: default_theme_name(),
        }
    }
}

fn default_theme_name() -> String {
    "dark".to_string()
}

impl TuiConfig {
    /// True when this `TuiConfig` equals its `Default`. Used by
    /// `#[serde(skip_serializing_if = ...)]` on `SpurConfig.tui` so existing
    /// configs do not gain a noisy `[tui]` block on round-trip.
    pub fn is_default(&self) -> bool {
        *self == TuiConfig::default()
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_dark_theme(value: &str) -> bool {
    value == "dark"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainConfig {
    /// Default brain agent name.
    #[serde(default = "default_brain")]
    pub default: String,
    /// Fallback brain agents, tried in order.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// Additional prompt context to inject.
    #[serde(default)]
    pub prompt: BrainPromptConfig,
    /// Feature flags for the delegation framework.
    #[serde(default)]
    pub delegation: BrainDelegationConfig,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            default: default_brain(),
            fallback: vec!["kiro".to_string()],
            prompt: BrainPromptConfig::default(),
            delegation: BrainDelegationConfig::default(),
        }
    }
}

fn default_brain() -> String {
    "claude-code".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainDelegationConfig {
    /// Which delegation framework version to use in the brain prompt.
    /// `"v1"` uses the rewritten prompt (workers block, dispatch
    /// procedure, delegation_plan guidance). `"legacy"` uses the
    /// pre-framework 5-line prose prompt. Build-aware default:
    /// debug builds default to `"v1"`; release builds default to
    /// `"legacy"` at v1 ship, flipping to `"v1"` at v2, removed at v3.
    pub framework: String,
}

impl Default for BrainDelegationConfig {
    fn default() -> Self {
        Self {
            framework: if cfg!(debug_assertions) {
                "v1".into()
            } else {
                "legacy".into()
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrainPromptConfig {
    /// Text appended to every brain prompt for this project.
    #[serde(default)]
    pub append: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsConfig {
    #[serde(default)]
    pub entries: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverConfig {
    /// Minutes to wait before retrying a rate-limited agent.
    #[serde(default = "default_cooldown")]
    pub cooldown_minutes: u64,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            cooldown_minutes: default_cooldown(),
        }
    }
}

fn default_cooldown() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeConfig {
    /// Maximum number of concurrent worktrees.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Hours after which stale worktrees are cleaned up.
    #[serde(default = "default_stale_hours")]
    pub stale_cleanup_hours: u64,
    /// Enable per-worker heartbeat watchdog. Default-off until the v1
    /// `_spur/heartbeat` emitter is available in production workers.
    #[serde(default = "default_worker_heartbeat_watchdog_enabled")]
    pub worker_heartbeat_watchdog_enabled: bool,
    /// Steady-state heartbeat timeout after dispatch.
    #[serde(default = "default_worker_heartbeat_timeout_secs")]
    pub worker_heartbeat_timeout_secs: u64,
    /// Startup grace before the first dispatch/heartbeat is observed.
    #[serde(default = "default_worker_heartbeat_initial_grace_secs")]
    pub worker_heartbeat_initial_grace_secs: u64,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            stale_cleanup_hours: default_stale_hours(),
            worker_heartbeat_watchdog_enabled: default_worker_heartbeat_watchdog_enabled(),
            worker_heartbeat_timeout_secs: default_worker_heartbeat_timeout_secs(),
            worker_heartbeat_initial_grace_secs: default_worker_heartbeat_initial_grace_secs(),
        }
    }
}

fn default_max_concurrent() -> usize {
    5
}

fn default_stale_hours() -> u64 {
    24
}

fn default_worker_heartbeat_watchdog_enabled() -> bool {
    false
}

fn default_worker_heartbeat_timeout_secs() -> u64 {
    90
}

fn default_worker_heartbeat_initial_grace_secs() -> u64 {
    60
}

fn default_event_replay_horizon_secs() -> u64 {
    7 * 86400
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing-subscriber` `EnvFilter` directives. `RUST_LOG` overrides if set.
    pub level: String,

    /// Per-chunk byte limit for `spur.log.YYYY-MM-DD.<n>`.
    pub max_file_bytes: u64,

    /// Number of rotated chunks to keep (`MaxFiles` argument to `file-rotate`).
    /// Active file + max_files = total chunks; total bytes ≤ (max_files+1) × max_file_bytes.
    pub max_files: usize,

    /// `tracing_appender::non_blocking` channel depth.
    pub buffered_lines_limit: usize,

    /// Per-child stderr bridge `non_blocking` channel depth.
    pub child_stderr_buffered_lines_limit: usize,

    /// Per-chunk byte limit for ACP child stderr files.
    pub child_stderr_max_bytes: u64,

    /// Number of rotated chunks per child to keep.
    pub child_stderr_max_files: usize,

    /// Total byte cap for `.spur/events/` ndjson directory.
    pub events_max_total_bytes: u64,

    /// How far back to replay NDJSON events on TUI startup, in seconds.
    /// Default 7 days. Bounds the cost of cold-start projection rehydration.
    #[serde(default = "default_event_replay_horizon_secs")]
    pub event_replay_horizon_secs: u64,

    /// If false, fall back to direct-FD stderr capture (legacy model, no rotation).
    pub child_stderr_pipe: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "warn,spur_core::orchestrator=info".to_string(),
            max_file_bytes: 8_388_608,
            max_files: 3,
            buffered_lines_limit: 8_192,
            child_stderr_buffered_lines_limit: 256,
            child_stderr_max_bytes: 2_621_440,
            child_stderr_max_files: 3,
            events_max_total_bytes: 67_108_864,
            event_replay_horizon_secs: 7 * 86400,
            child_stderr_pipe: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostConfig {
    /// Path to the SQLite cost database.
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
        }
    }
}

fn default_db_path() -> String {
    "~/.spur/cost.db".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PmConfig {
    #[serde(default)]
    pub github: Option<GitHubPmConfig>,
    #[serde(default)]
    pub beads: Option<BeadsPmConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub use_gh_cli: bool,
    /// Repository in "owner/repo" format.
    pub repo: Option<String>,
    /// Label to auto-add to SPUR-managed issues.
    #[serde(default = "default_auto_label")]
    pub auto_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadsPmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
}

fn default_true() -> bool {
    true
}

fn default_auto_label() -> String {
    "spur-managed".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
}

// ─── Duration serde helper ─────────────────────────────────────────────

mod optional_duration_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => serializer.serialize_str(&format!("{}s", d.as_secs())),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: Option<String> = Option::deserialize(deserializer)?;
        match s {
            None => Ok(None),
            Some(s) => {
                let s = s.trim();
                if let Some(secs) = s.strip_suffix('s') {
                    let secs: u64 = secs.parse().map_err(serde::de::Error::custom)?;
                    Ok(Some(Duration::from_secs(secs)))
                } else if let Some(mins) = s.strip_suffix('m') {
                    let mins: u64 = mins.parse().map_err(serde::de::Error::custom)?;
                    Ok(Some(Duration::from_secs(mins * 60)))
                } else if let Some(hours) = s.strip_suffix('h') {
                    let hours: u64 = hours.parse().map_err(serde::de::Error::custom)?;
                    Ok(Some(Duration::from_secs(hours * 3600)))
                } else {
                    Err(serde::de::Error::custom(
                        "expected duration like '30s', '5m', or '2h'",
                    ))
                }
            }
        }
    }
}

/// Embedded seed template. Parsed by `load_seed_template()`. Source of
/// truth is `crates/spur-acp/src/seed_agents.toml`.
const SEED_TOML: &str = include_str!("../seed_agents.toml");

/// Parse the embedded seed template. Returns the pre-known agent set
/// that `spur init` discovers on $PATH.
///
/// Errors are unreachable in production thanks to the compile-time
/// seed-template parse test below. If a
/// maintainer skips tests and commits a bad edit, users see a clear
/// diagnostic instead of a raw panic.
pub fn load_seed_template() -> AgentsConfig {
    #[derive(serde::Deserialize)]
    struct SeedFile {
        agents: AgentsConfig,
    }
    let parsed: SeedFile = toml::from_str(SEED_TOML).unwrap_or_else(|e| {
        panic!(
            "embedded seed_agents.toml failed to parse (this is a spur bug, \
             please report): {e}"
        )
    });
    parsed.agents
}

/// Atomically read-modify-write a `SpurConfig` on disk.
///
/// Reads `path`, deserializes into `SpurConfig`, applies `mutate`, serializes
/// via `toml::to_string_pretty`, writes to a `NamedTempFile` in the same
/// directory, fsyncs the file, atomically renames over `path`, and (on Unix)
/// fsyncs the parent directory so the rename's directory-entry update is
/// durable across crashes.
///
/// Errors:
/// - `path` does not exist → returns the underlying read error.
/// - `path` contains invalid TOML → returns the parse error; original file
///   is left untouched.
/// - tempfile/write/fsync/rename failures are propagated with context.
///
/// Concurrency: two concurrent callers will produce a last-rename-wins
/// outcome. This is acceptable for preference-class fields; do NOT use this
/// helper for fields requiring CAS semantics.
pub fn update_config<F>(path: &std::path::Path, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut SpurConfig),
{
    use anyhow::Context;
    use std::io::Write;

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read config at {}", path.display()))?;
    let mut cfg: SpurConfig =
        toml::from_str(&raw).with_context(|| format!("parse config at {}", path.display()))?;
    mutate(&mut cfg);
    let serialized = toml::to_string_pretty(&cfg).context("serialize SpurConfig to TOML")?;

    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("create tempfile in {}", dir.display()))?;
    tmp.write_all(serialized.as_bytes())
        .context("write serialized config to tempfile")?;
    tmp.as_file()
        .sync_all()
        .context("fsync tempfile before rename")?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("atomic rename to {}", path.display()))?;
    // Fsync the parent directory so the rename's directory-entry update is
    // durable. POSIX `rename(2)` is atomic for visibility, but the directory
    // inode block reaching disk requires an explicit fsync on the dir.
    // Unix-only: Windows rejects opening a directory for sync.
    #[cfg(unix)]
    std::fs::File::open(dir)
        .with_context(|| format!("open config directory {} for fsync", dir.display()))?
        .sync_all()
        .with_context(|| format!("fsync config directory {}", dir.display()))?;
    Ok(())
}

#[cfg(test)]
mod delegation_descriptor_tests {
    use super::*;

    #[test]
    fn descriptor_deserializes_from_partial_toml() {
        let toml = r#"
            description = "test agent"
            tier = "specialist"
            good_for = ["a", "b"]
        "#;
        let d: DelegationDescriptor = toml::from_str(toml).unwrap();
        assert_eq!(d.description.as_deref(), Some("test agent"));
        assert!(matches!(d.tier, Some(Tier::Specialist)));
        assert_eq!(d.good_for, vec!["a".to_string(), "b".to_string()]);
        assert!(d.avoid_for.is_empty());
        assert!(d.inherit_defaults); // default true
    }

    #[test]
    fn descriptor_default_is_empty_and_inherits() {
        let d = DelegationDescriptor::default();
        assert!(d.description.is_none());
        assert!(d.good_for.is_empty());
        assert!(d.inherit_defaults);
    }

    #[test]
    fn agent_config_parses_delegation_sub_table() {
        let toml = r#"
            name = "claude-x"
            command = "claude"
            transport = "acp"

            [delegation]
            description = "custom claude variant"
            good_for = ["one-offs"]
        "#;
        let cfg: AgentConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            cfg.delegation.description.as_deref(),
            Some("custom claude variant")
        );
        assert_eq!(cfg.delegation.good_for, vec!["one-offs".to_string()]);
    }

    #[test]
    fn agent_config_without_delegation_block_uses_defaults() {
        let toml = r#"
            name = "bare"
            command = "bare"
            transport = "acp"
        "#;
        let cfg: AgentConfig = toml::from_str(toml).unwrap();
        assert!(cfg.delegation.description.is_none());
        assert!(cfg.delegation.inherit_defaults);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_handle_prefers_display_handle() {
        let mut cfg = AgentConfig::with_defaults("ClaudeCode");
        cfg.display.handle = Some("cc".into());
        assert_eq!(cfg.effective_handle(), "cc");
    }

    #[test]
    fn effective_handle_falls_back_to_lowercased_name() {
        let cfg = AgentConfig::with_defaults("ClaudeCode");
        assert_eq!(cfg.effective_handle(), "claudecode");
    }

    #[test]
    fn seed_template_parses_and_covers_shipped_agents() {
        let seeds = load_seed_template();
        const EXPECTED_SEED_AGENTS: &[&str] = &[
            "kiro",
            "claude-code",
            "codex-bin",
            "codex",
            "gemini",
            "opencode",
        ];
        assert!(
            seeds.entries.len() >= EXPECTED_SEED_AGENTS.len(),
            "seed template must have ≥{} agents, got {}",
            EXPECTED_SEED_AGENTS.len(),
            seeds.entries.len()
        );
        let names: Vec<_> = seeds.entries.iter().map(|a| a.name.as_str()).collect();
        for expected in EXPECTED_SEED_AGENTS {
            assert!(
                names.contains(expected),
                "missing seed agent: {expected} (got {names:?})"
            );
        }
    }

    #[test]
    fn seed_template_codex_has_static_commands() {
        let seeds = load_seed_template();
        let codex = seeds
            .entries
            .iter()
            .find(|a| a.name == "codex")
            .expect("codex should be in seed template");
        assert!(
            !codex.commands.static_commands.is_empty(),
            "codex must have at least one static command (proves Spec 2)"
        );
    }

    #[test]
    fn seed_template_kimi_uses_yolo_via_l2_auto_approve() {
        // kimi requires `-y` BEFORE the `acp` subcommand and relies on the
        // L2 auto-approve path (orchestrator drops `permission_tx` when
        // `effective_permissions().skip == true`) for the residual
        // shell-command permission prompt that `-y` does NOT suppress.
        // This locks both the argv order and the L2 opt-in — a
        // future edit that breaks either should fail this test.
        let seeds = load_seed_template();
        let kimi = seeds
            .entries
            .iter()
            .find(|a| a.name == "kimi")
            .expect("kimi should be in seed template");
        assert_eq!(kimi.kind, crate::types::AgentKind::Kimi);
        assert_eq!(
            kimi.effective_args(),
            vec!["-y".to_string(), "--afk".to_string(), "acp".to_string()],
            "`-y --afk` must precede `acp`: `-y` is the yolo flag, `--afk` \
             is required on kimi 1.40.0+ because `-y` alone silences \
             agent_message_chunk in ACP mode (empirically verified via \
             scripts/simulate_kimi_acp.py)"
        );
        assert!(
            kimi.effective_permissions().skip,
            "kimi must opt into L2 auto-approve via permissions.skip = true"
        );
        // Validator must accept kimi (R3 is a warning, not fatal) and the
        // single error reported must be the expected R3 — proving the
        // mechanism is intentionally L2-only.
        let errs = crate::config::validate_agent_config(kimi)
            .expect_err("expected R3 warning since mechanism is L2 auto-approve only");
        assert_eq!(errs.len(), 1, "only R3 should fire: {errs:?}");
        assert!(!errs[0].is_fatal(), "R3 must remain non-fatal");
        assert!(
            matches!(
                errs[0],
                crate::config::ConfigError::SkipPermissionsNoExplicitMechanism { .. }
            ),
            "expected R3 SkipPermissionsNoExplicitMechanism, got {:?}",
            errs[0]
        );
    }

    #[test]
    fn seed_template_gemini_uses_gemini_kind() {
        let seeds = load_seed_template();
        let gemini = seeds
            .entries
            .iter()
            .find(|a| a.name == "gemini")
            .expect("gemini should be in seed template");
        assert_eq!(gemini.kind, crate::types::AgentKind::Gemini);
    }

    #[test]
    fn brain_delegation_framework_defaults_per_build() {
        // Empty [brain.delegation] block → build-aware default.
        let toml = r#"
            [brain]
            default = "claude-code"
        "#;
        let cfg: BrainConfig = toml::from_str(toml).unwrap();
        let expected = if cfg!(debug_assertions) {
            "v1"
        } else {
            "legacy"
        };
        assert_eq!(cfg.delegation.framework, expected);
    }

    #[test]
    fn brain_delegation_framework_explicit_v1() {
        let toml = r#"
            default = "claude-code"
            [delegation]
            framework = "v1"
        "#;
        let cfg: BrainConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delegation.framework, "v1");
    }

    #[test]
    fn seed_template_passes_validator() {
        let seeds = load_seed_template();
        for agent in &seeds.entries {
            let errs = crate::config::validate_agent_config(agent);
            let fatal: Vec<_> = errs
                .err()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.is_fatal())
                .collect();
            assert!(
                fatal.is_empty(),
                "seed agent `{}` has fatal validator errors: {fatal:?}",
                agent.name
            );
        }
    }

    #[test]
    fn spur_runtime_defaults_auto_merge_to_false() {
        let cfg: SpurConfig = toml::from_str("").unwrap();
        assert!(!cfg.spur.auto_merge_approved_plans);
        assert_eq!(cfg.spur.plan_pending_grace_secs, 60 * 60);
        assert_eq!(cfg.spur.dispatch_lease_secs, 600);
        assert_eq!(cfg.spur.dispatch_lease_heartbeat_secs, 200);
    }

    #[test]
    fn spur_config_default_has_peer_mailbox_disabled() {
        let cfg = SpurConfig::default();
        assert!(!cfg.peer_mailbox_enabled);

        let parsed: SpurConfig = toml::from_str("").unwrap();
        assert!(!parsed.peer_mailbox_enabled);
    }

    #[test]
    fn worktree_defaults_have_heartbeat_watchdog_disabled() {
        let cfg = SpurConfig::default();
        assert!(!cfg.worktree.worker_heartbeat_watchdog_enabled);
        assert_eq!(cfg.worktree.worker_heartbeat_timeout_secs, 90);
        assert_eq!(cfg.worktree.worker_heartbeat_initial_grace_secs, 60);

        let parsed: SpurConfig = toml::from_str("").unwrap();
        assert!(!parsed.worktree.worker_heartbeat_watchdog_enabled);
        assert_eq!(parsed.worktree.worker_heartbeat_timeout_secs, 90);
        assert_eq!(parsed.worktree.worker_heartbeat_initial_grace_secs, 60);
    }

    #[test]
    fn spur_runtime_parses_auto_merge_true() {
        let cfg: SpurConfig = toml::from_str(
            r#"
            [spur]
            auto_merge_approved_plans = true
            plan_pending_grace_secs = 10
            dispatch_lease_secs = 30
            dispatch_lease_heartbeat_secs = 10
            "#,
        )
        .unwrap();
        assert!(cfg.spur.auto_merge_approved_plans);
        assert_eq!(cfg.spur.plan_pending_grace_secs, 10);
        assert_eq!(cfg.spur.dispatch_lease_secs, 30);
        assert_eq!(cfg.spur.dispatch_lease_heartbeat_secs, 10);
    }

    #[test]
    fn parses_log_section_with_defaults() {
        let toml_str = r#"
[log]
level = "warn,spur_core::orchestrator=info"
"#;
        let cfg: SpurConfig = toml::from_str(toml_str).expect("valid config");
        assert_eq!(cfg.log.level, "warn,spur_core::orchestrator=info");
        assert_eq!(cfg.log.max_file_bytes, 8_388_608); // 8 MB
        assert_eq!(cfg.log.max_files, 3);
        assert_eq!(cfg.log.buffered_lines_limit, 8_192);
        assert_eq!(cfg.log.child_stderr_buffered_lines_limit, 256);
        assert_eq!(cfg.log.child_stderr_max_bytes, 2_621_440); // 2.5 MB
        assert_eq!(cfg.log.child_stderr_max_files, 3);
        assert_eq!(cfg.log.events_max_total_bytes, 67_108_864); // 64 MB
        assert!(cfg.log.child_stderr_pipe);
    }

    #[test]
    fn log_config_default_event_replay_horizon_is_seven_days() {
        let cfg = LogConfig::default();
        assert_eq!(cfg.event_replay_horizon_secs, 7 * 86400);
    }

    #[test]
    fn parses_empty_config_uses_log_defaults() {
        let cfg: SpurConfig = toml::from_str("").expect("empty config valid");
        assert_eq!(cfg.log.level, "warn,spur_core::orchestrator=info");
        assert_eq!(cfg.log.max_file_bytes, 8_388_608);
        assert_eq!(cfg.log.child_stderr_buffered_lines_limit, 256);
    }
}
