use crate::domain::delegation::TimeoutFallback;
use crate::types::{AgentRole, CostTier, TransportKind};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

/// Global SPUR configuration (from ~/.spur/config.toml + .spur/config.toml).
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
    pub project: Option<ProjectConfig>,
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
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            default: default_brain(),
            fallback: vec!["kiro".to_string()],
            prompt: BrainPromptConfig::default(),
        }
    }
}

fn default_brain() -> String {
    "claude-code".to_string()
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
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            stale_cleanup_hours: default_stale_hours(),
        }
    }
}

fn default_max_concurrent() -> usize {
    5
}

fn default_stale_hours() -> u64 {
    24
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
