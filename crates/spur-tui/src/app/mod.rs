//! App orchestration root. This module keeps the `App` state, construction,
//! event tick, render dispatch, and still-unextracted handlers. Submodules own
//! thematic `impl App` blocks such as analytics initialization and live-cost
//! refresh plumbing.

mod action_routing;
mod analytics;
mod events;
mod input;
mod navigation;
mod overlays;

pub(crate) use events::apply_session_update;
use overlays::render_user_warning;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
#[cfg(feature = "analytics")]
use tokio::sync::RwLock;
use tokio::sync::{broadcast, mpsc};
#[cfg(feature = "analytics")]
use tokio::task::JoinHandle;
use tokio::time::timeout;

use spur_acp::domain::events::BrainRetireReason;
use spur_acp::{
    LicenseBindingMode, LicensePlan as EventLicensePlan, LicenseStateEvent, LicenseStatusEvent,
    LicenseSubjectKind, SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};

#[cfg(feature = "markdown")]
use ratatui_image::picker::Picker;

use crate::action::{Action, ViewId};
use crate::components::collision_modal::CollisionModal;
use crate::components::help_overlay::HelpOverlay;
use crate::components::input_bar::EditMode;
use crate::components::palette::PaletteIntent;
use crate::components::palette_sources::{
    CommandSource, PaletteSource, SessionSource, ViewSource, WorkerSource,
};
use crate::components::quit_confirm::QuitConfirmDialog;
use crate::components::status_bar::{HintOverride, LicenseBadge, LicenseBadgeTone};
use crate::components::tombstone::{Tombstone, TombstoneKind};
use crate::components::upgrade_modal::{self, UpgradeModalState};
use crate::input_history::{InputHistoryEntry, HISTORY_CAP};
use crate::session_metadata::{ReadOnlyFutureSchema, SessionMetadataStore};
use crate::tui;
use crate::views::dashboard::{DashboardMode, DashboardView};
use crate::views::issue_browser::IssueBrowserView;
use crate::views::plan_browser::PlanBrowserView;
use crate::views::plan_inspector::PlanInspectorView;
use crate::views::session_detail::SessionDetailView;
use crate::views::session_picker::SessionPickerView;
use crate::views::View;

const READ_ONLY_STARTUP_WARNING: &str =
    "Read-only mode: session metadata was written by a newer SPUR. \
Edits this session WILL NOT be persisted. Upgrade SPUR to enable writes. (Esc to dismiss)";
const LEGACY_ARCHIVE_HINT: &str = "d \u{2192} archive renamed to x";
const LEGACY_CLOSE_HINT: &str = "d \u{2192} close renamed to x";
const DASHBOARD_TAB_DEPRECATION_HINT: &str =
    "Tab now cycles panels; press Ctrl+E to cycle examples";
const PANIC_RESET_HINT: &str = "Returned to Dashboard root";
const EXECUTE_EDIT_HINT: &str = "Prompt loaded \u{2014} review and press Enter to send";
const PANIC_RESET_ESC_WINDOW: Duration = Duration::from_millis(1000);

// ─── Supporting types ──────────────────────────────────────────────────

pub struct TransientHint {
    pub text: String,
    pub expires_at: Instant,
}

/// A user input message or control command sent from the TUI to the backend.
pub enum UserInput {
    Message {
        session: SessionId,
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a new brain session and send these blocks as the first prompt
    /// atomically. Emitted by the TUI when the user types into Dashboard's
    /// InputBar with no brain attached, or from the picker's
    /// NewSessionRequested path.
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    /// Request the orchestrator to call `set_session_mode` on the current
    /// brain session with the given mode id (e.g. `"plan"`, `"default"`).
    SetSessionMode {
        mode_id: String,
    },
    SubmitReview {
        executor_id: String,
        /// The attempt_n from the pending review card the user acted on.
        /// The orchestrator's dispatcher uses this as a supersession guard.
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
    /// Invoke an agent vendor-extension RPC on the active brain session.
    VendorExec {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    /// Set an ACP session config option (v1 codex /model and /effort).
    /// Maps 1:1 to `spur_core::InteractiveInput::SetSessionConfigOption`.
    SetSessionConfigOption {
        config_id: String,
        value: String,
    },
    /// Dedicated `session/set_model` dispatch (M9 F-C). Maps 1:1 to
    /// `spur_core::InteractiveInput::SetSessionModel`. The orchestrator
    /// delegates to `AgentConnection::set_session_model`, which owns the
    /// dispatch decision (Direct / FallbackConfigOption / Unsupported).
    SetSessionModel {
        session_id: SessionId,
        value: String,
    },
    /// Halt the in-flight agent stream on the given session. Maps 1:1 to
    /// `spur_core::InteractiveInput::CancelStream` via `spur-cli`.
    CancelStream {
        session: SessionId,
    },
    /// Request the orchestrator to refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Request the orchestrator to refresh persisted plan summaries.
    RefreshPlans,
    /// Request the orchestrator to claim a persisted plan without starting execution.
    ClaimPlan {
        plan_id: String,
    },
    /// Request the orchestrator to resume a persisted plan.
    ResumePlan {
        plan_id: String,
    },
    /// Request a read-only persisted implementation-plan snapshot.
    InspectPlan {
        plan_id: String,
    },
    /// Request full issue detail from the PM backend.
    GetIssueDetail {
        id: String,
    },
    /// Request the dependency subgraph around an issue from the PM backend.
    GetIssueGraph {
        id: String,
    },
    /// Update an issue's status/assignee/labels via PM backend.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
}

/// Tracks the brain agent's current state for status indicators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrainStatus {
    Idle,
    Connecting,
    Connected,
    Thinking,
    Streaming,
    Ready,
    Error(String),
}

fn format_plan(plan: EventLicensePlan) -> &'static str {
    match plan {
        EventLicensePlan::Community => "community",
        EventLicensePlan::StarterLtd => "starter-ltd",
        EventLicensePlan::BuilderLtd => "builder-ltd",
        EventLicensePlan::FounderLtd => "founder-ltd",
        EventLicensePlan::Pro => "pro",
        EventLicensePlan::Team => "team",
        EventLicensePlan::Enterprise => "enterprise",
        EventLicensePlan::Unknown => "unknown",
    }
}

fn license_badge_from_state(state: &LicenseStateEvent) -> Option<LicenseBadge> {
    use LicenseStatusEvent::*;

    match state.status {
        ConfigError => Some(LicenseBadge::new(
            "license config",
            LicenseBadgeTone::Danger,
        )),
        Inactive => Some(LicenseBadge::new("community", LicenseBadgeTone::Neutral)),
        Invalid => Some(LicenseBadge::new("invalid", LicenseBadgeTone::Danger)),
        Degraded => {
            let label = format!("{} degraded", format_plan(state.plan));
            Some(LicenseBadge::new(label, LicenseBadgeTone::Warning))
        }
        Active => {
            let label = if matches!(state.plan, EventLicensePlan::Unknown) {
                "licensed".to_string()
            } else {
                format_plan(state.plan).to_string()
            };
            Some(LicenseBadge::new(label, LicenseBadgeTone::Success))
        }
    }
}

/// Convert the ACP broadcast representation into the license resolver input.
/// This is the inverse of `spur_core::license_runtime::to_event_state`.
fn license_state_event_to_state(state: &LicenseStateEvent) -> spur_license::LicenseState {
    spur_license::LicenseState {
        status: match state.status {
            LicenseStatusEvent::Inactive => spur_license::LicenseStatus::Inactive,
            LicenseStatusEvent::Active => spur_license::LicenseStatus::Active,
            LicenseStatusEvent::Degraded => spur_license::LicenseStatus::Degraded,
            LicenseStatusEvent::Invalid => spur_license::LicenseStatus::Invalid,
            LicenseStatusEvent::ConfigError => spur_license::LicenseStatus::ConfigError,
        },
        subject_kind: match state.subject_kind {
            LicenseSubjectKind::User => spur_license::SubjectKind::User,
            LicenseSubjectKind::Organization => spur_license::SubjectKind::Organization,
            LicenseSubjectKind::Ci => spur_license::SubjectKind::Ci,
            LicenseSubjectKind::Unknown => spur_license::SubjectKind::Unknown,
        },
        plan: match state.plan {
            EventLicensePlan::Community => spur_license::Plan::Community,
            EventLicensePlan::StarterLtd => spur_license::Plan::StarterLtd,
            EventLicensePlan::BuilderLtd => spur_license::Plan::BuilderLtd,
            EventLicensePlan::FounderLtd => spur_license::Plan::FounderLtd,
            EventLicensePlan::Pro => spur_license::Plan::Pro,
            EventLicensePlan::Team => spur_license::Plan::Team,
            EventLicensePlan::Enterprise => spur_license::Plan::Enterprise,
            EventLicensePlan::Unknown => spur_license::Plan::Unknown,
        },
        features: state.features.clone(),
        expires_at: state.expires_at,
        binding_mode: match state.binding_mode {
            LicenseBindingMode::NodeLocked => spur_license::BindingMode::NodeLocked,
            LicenseBindingMode::FloatingCi => spur_license::BindingMode::FloatingCi,
            LicenseBindingMode::Organization => spur_license::BindingMode::Organization,
            LicenseBindingMode::Unknown => spur_license::BindingMode::Unknown,
        },
        offline_ok: state.offline_ok,
        status_text: state.status_text.clone(),
    }
}

fn is_placeholder_license_state(state: &LicenseStateEvent) -> bool {
    matches!(state.status, LicenseStatusEvent::Inactive)
        && matches!(state.subject_kind, LicenseSubjectKind::Unknown)
        && matches!(state.plan, EventLicensePlan::Unknown)
        && state.features.is_empty()
        && matches!(state.binding_mode, LicenseBindingMode::Unknown)
        && !state.offline_ok
        && state.status_text == PLACEHOLDER_STATUS_TEXT
}

fn compute_flag_summary() -> Option<(usize, usize)> {
    use spur_license::policy::PolicyResolver;
    use spur_license::{FeatureGate, FlagKey};

    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    let flags = [
        FlagKey::KILL_ADVANCED_PLANNER,
        FlagKey::ENABLE_BROWSER_TOOL,
        FlagKey::ENABLE_COMPACTION_V2,
        FlagKey::ENABLE_TELEMETRY,
        FlagKey::ENABLE_V1_1_PREVIEW,
    ];

    let total = flags.len();
    let active = flags
        .iter()
        .filter(|&&k| gate.is_flag_enabled(k).unwrap_or(false))
        .count();

    Some((active, total))
}

// ─── App state ─────────────────────────────────────────────────────────

#[cfg(feature = "analytics")]
use analytics::InsightsInitState;
#[cfg(feature = "analytics")]
pub use analytics::LiveCostCache;

/// Inc 2 (bd-d587.2): cap on `App::view_history`. Sized for typical TUI nav
/// depth (Dashboard → SessionDetail → PlanBrowser → PlanInspector / IssueBrowser);
/// older entries fall off the front when exceeded so cyclic navigation can't
/// grow unboundedly.
const NAV_HISTORY_MAX: usize = 16;

pub struct App {
    pub(super) current_view: ViewId,
    /// Inc 2 (bd-d587.2): bounded LIFO of recently-left views, capped at
    /// `NAV_HISTORY_MAX`. `navigate_to` pushes the leaving view; `navigate_back`
    /// pops it. Dashboard is the canonical root and clears the stack on entry.
    /// Modal overlays (palette, help, modals) never touch this stack.
    view_history: Vec<ViewId>,
    pub(super) dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    session_picker: Option<SessionPickerView>,
    plan_browser: Option<PlanBrowserView>,
    plan_inspector: Option<PlanInspectorView>,
    issue_browser: Option<IssueBrowserView>,
    help_visible: bool,
    /// Shown when the user requests quit while a brain is attached. While
    /// visible, all input is captured by the dialog.
    quit_confirm_visible: bool,
    /// Visible when a resume attempt collides with another attached TUI.
    collision_modal: Option<CollisionModalState>,
    /// Plan C Tier 2 — capability-tease modal shown when a TUI-side
    /// feature gate denies. Owned (not borrowed) because the modal
    /// outlives the gate-check call site. `FeatureGateError: Clone`
    /// (Task 1) makes that ownership cheap.
    upgrade_modal: Option<UpgradeModalState>,
    should_quit: bool,
    pub(super) dirty: bool,
    /// Top-level user-visible warning banner rendered in a reserved top row.
    user_warning: Option<String>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    #[cfg(any(test, debug_assertions))]
    user_input_rx_for_test: Option<mpsc::Receiver<UserInput>>,
    brain_status: BrainStatus,
    brain_name: Option<String>,
    pending_first_user_message: Option<String>,
    pending_permission: Option<(spur_acp::types::PermissionRequest, std::time::Instant)>,
    /// Event-sourced projection of brain → executor lineage.
    pub(super) lineage: ExecutorLineage,
    #[cfg(feature = "analytics")]
    pub(super) analytics_engine: Option<spur_context::AsyncEngine>,
    #[cfg(feature = "analytics")]
    pub(super) live_cost_cache: Option<std::sync::Arc<RwLock<LiveCostCache>>>,
    #[cfg(feature = "analytics")]
    pub(super) live_cost_active_sessions:
        Option<std::sync::Arc<RwLock<std::collections::HashSet<SessionId>>>>,
    #[cfg(feature = "analytics")]
    pub(super) live_cost_signal_tx: Option<mpsc::Sender<()>>,
    #[cfg(feature = "analytics")]
    pub(super) live_cost_handle: Option<JoinHandle<()>>,
    /// Lazily constructed on first `Action::OpenInsights`. None until the
    /// user presses Alt+a (or `analytics_engine` is otherwise initialised).
    #[cfg(feature = "analytics")]
    pub(super) insights_view: Option<crate::views::insights::InsightsView>,
    /// In-flight cold-init for the analytics engine. While `Some`, the
    /// Insights view renders an "indexing logs..." placeholder; the
    /// `oneshot::Receiver` is polled on tick and resolves to either the
    /// constructed `AsyncEngine` (success) or an error string (failure).
    /// Cleared once the outcome is consumed in either branch. Bug A:
    /// without this the init ran synchronously on the UI thread for ~89s
    /// on first open, freezing the entire TUI.
    #[cfg(feature = "analytics")]
    pub(super) insights_init: Option<InsightsInitState>,
    /// Durable plan snapshots keyed by session and plan id.
    plan_projection: PlanProjectionStore,
    synopsis: SessionSynopsisProjection,
    /// Per-executor `ReactTrace` instances rendered by the Stream tab.
    /// Populated on every `SpurEventBody::WorkerNotification`.
    pub(crate) worker_streams: crate::worker_streams::WorkerStreams,
    license_state: LicenseStateEvent,
    license_badge: Option<LicenseBadge>,
    flag_summary: Option<(usize, usize)>, // (active_count, total_count)
    /// Plan C Tier 2 — long-lived feature-gate snapshot reflecting the
    /// embedded policy (community baseline + `SPUR_LICENSE_TEST_STRIP_KEYS`).
    /// Used by the MVP gate-check site at `Action::SendMessage`. Future
    /// M1 work will pump live `LicenseStateEvent` updates into this gate
    /// via `update_state` so Pro-only gate sites resolve correctly.
    feature_gate: spur_license::FeatureGate,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_picker: Option<Picker>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_rx: tokio::sync::mpsc::UnboundedReceiver<Action>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_tx: tokio::sync::mpsc::UnboundedSender<Action>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_viewer: Option<crate::views::mermaid_viewer::MermaidViewerView>,
    metadata_store: SessionMetadataStore,
    /// Current input editing mode, synced across all InputBar instances.
    edit_mode: EditMode,
    /// Per-view tombstone slots for Gmail-toast-style destructive-action undo.
    /// Driven by tick; install points live in process_action arms.
    tombstones: crate::components::tombstone::TombstoneSlots,
    /// Suppresses tombstone re-install while an undo inverse is replayed.
    tombstone_undo_replay: bool,
    /// Loaded Spur configuration. Used to resolve per-agent `AgentConfig`
    /// at session-creation time (see `resolve_agent_config`). Defaults to
    /// `SpurConfig::default()` when no config is supplied.
    config: std::sync::Arc<spur_acp::SpurConfig>,
    /// Path to the project-local `.spur/config.toml` that seeded `config`.
    /// When `Some`, `/theme <name>` persists the theme choice back to this
    /// file via `update_config`. `None` in test fixtures and when the TUI
    /// is launched without a discoverable repo-local config.
    config_path: Option<std::path::PathBuf>,
    /// Active theme resolved at startup from `config.tui.theme` via the
    /// project → user → built-in cascade. Surfaces read tokens off this
    /// reference; the dark built-in is a pixel-perfect reproduction of
    /// pre-theme TUI colors so unmigrated `Color::` sites stay visually
    /// stable until PR3/PR4 swap them out.
    pub(crate) theme: std::sync::Arc<crate::theme::Theme>,
    /// Theme name as requested via `config.tui.theme` or the most recent
    /// successful `/theme <name>` switch. Distinct from `theme.name`,
    /// which reflects the YAML-declared name (a project file can rename
    /// itself). `/theme reload` re-resolves this string against the cascade.
    pub(crate) active_theme_name: String,
    palette_visible: bool,
    palette_state: crate::components::palette::PaletteState,
    pub transient_hint: Option<TransientHint>,
    legacy_archive_hint_shown: bool,
    legacy_issue_close_hint_shown: bool,
    dashboard_tab_empty_deprecation_shown: bool,
    esc_chain: VecDeque<Instant>,
    /// Startup landing decision. Drives initial view and banner state.
    landing: crate::landing::LandingDecision,
    /// Last dispatched Action, for integration tests only.
    #[cfg(any(test, debug_assertions))]
    last_action: Option<crate::action::Action>,
}

#[derive(Debug, Clone)]
struct CollisionModalState {
    acp_id: String,
    holder: spur_acp::session_lock::HolderInfo,
}

/// Sentinel status_text used by `default_license_state` to identify
/// the App-internal placeholder license state (vs. real provider
/// states that may also be Inactive). Used by `is_placeholder_license_state`
/// to skip startup-hydration of `feature_gate` when no real license
/// has been seeded yet.
const PLACEHOLDER_STATUS_TEXT: &str = "licensing not configured";

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>, start_in_picker: bool) -> Self {
        Self::new_with_config(
            user_input_tx,
            start_in_picker,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            crate::landing::LandingDecision::ShowDashboard,
        )
    }

    fn default_license_state(message: &str) -> LicenseStateEvent {
        LicenseStateEvent {
            status: LicenseStatusEvent::Inactive,
            subject_kind: LicenseSubjectKind::Unknown,
            plan: EventLicensePlan::Unknown,
            features: Default::default(),
            expires_at: None,
            binding_mode: LicenseBindingMode::Unknown,
            offline_ok: false,
            status_text: message.to_string(),
        }
    }

    pub fn new_with_license(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker: bool,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self::build_with_license_state(
            user_input_tx,
            start_in_picker.then_some(None),
            config,
            license_state,
            landing,
            config_path,
        )
    }

    pub fn new_with_config(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker: bool,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        landing: crate::landing::LandingDecision,
    ) -> Self {
        Self::new_with_license(
            user_input_tx,
            start_in_picker,
            config,
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            landing,
            None,
        )
    }

    fn build_with_license_state(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        let metadata_path = std::path::PathBuf::from(".spur").join("session_metadata.json");
        Self::build_with_license_state_from_metadata_path(
            user_input_tx,
            start_in_picker_with_preselect,
            config,
            license_state,
            landing,
            metadata_path,
            config_path,
        )
    }

    fn build_with_license_state_from_metadata_path(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        metadata_path: std::path::PathBuf,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        let metadata_store = SessionMetadataStore::load(&metadata_path);
        let start_in_picker = start_in_picker_with_preselect.is_some();

        // Resolve the active theme from `tui.theme` config. The runtime
        // loader logs and falls back internally; it never panics.
        let active_theme_name = config.tui.theme.clone();
        let (theme, theme_outcome) = crate::theme::load_runtime_theme(&active_theme_name);
        tracing::info!(
            target: "spur_tui::theme",
            theme = %theme.name,
            outcome = ?theme_outcome,
            "active theme resolved"
        );
        let theme = std::sync::Arc::new(theme);

        let (current_view, session_picker) = if let Some(preselect) = start_in_picker_with_preselect
        {
            let mut picker = SessionPickerView::with_preselect(preselect);
            picker.set_metadata(metadata_store.metadata().clone());
            (ViewId::SessionPicker, Some(picker))
        } else {
            (ViewId::Dashboard, None)
        };

        #[cfg(feature = "markdown")]
        let mermaid_picker = Picker::from_query_stdio().ok();
        #[cfg(feature = "markdown")]
        let (mermaid_tx, mermaid_rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "analytics")]
        let live_cost_cache = std::sync::Arc::new(RwLock::new(LiveCostCache::default()));
        #[cfg(feature = "analytics")]
        let live_cost_active_sessions =
            std::sync::Arc::new(RwLock::new(std::collections::HashSet::new()));
        #[cfg(feature = "analytics")]
        let dashboard = DashboardView::with_cache(live_cost_cache.clone());
        #[cfg(not(feature = "analytics"))]
        let dashboard = DashboardView::new();

        let mut app = Self {
            current_view,
            view_history: Vec::new(),
            dashboard,
            session_detail: None,
            session_picker,
            plan_browser: None,
            plan_inspector: None,
            issue_browser: None,
            help_visible: false,
            quit_confirm_visible: false,
            collision_modal: None,
            upgrade_modal: None,
            should_quit: false,
            dirty: true, // initial render
            user_warning: None,
            user_input_tx,
            #[cfg(any(test, debug_assertions))]
            user_input_rx_for_test: None,
            brain_status: BrainStatus::Idle,
            brain_name: None,
            pending_first_user_message: None,
            pending_permission: None,
            lineage: ExecutorLineage::new(),
            #[cfg(feature = "analytics")]
            analytics_engine: None,
            #[cfg(feature = "analytics")]
            live_cost_cache: Some(live_cost_cache),
            #[cfg(feature = "analytics")]
            live_cost_active_sessions: Some(live_cost_active_sessions),
            #[cfg(feature = "analytics")]
            live_cost_signal_tx: None,
            #[cfg(feature = "analytics")]
            live_cost_handle: None,
            #[cfg(feature = "analytics")]
            insights_view: None,
            #[cfg(feature = "analytics")]
            insights_init: None,
            plan_projection: PlanProjectionStore::new(),
            synopsis: SessionSynopsisProjection::new(),
            worker_streams: crate::worker_streams::WorkerStreams::new(),
            #[cfg(feature = "markdown")]
            mermaid_picker,
            #[cfg(feature = "markdown")]
            mermaid_rx,
            #[cfg(feature = "markdown")]
            mermaid_tx,
            #[cfg(feature = "markdown")]
            mermaid_viewer: None,
            license_state,
            license_badge: None,
            flag_summary: None,
            feature_gate: spur_license::FeatureGate::new(
                spur_license::policy::PolicyResolver::embedded(),
            ),
            metadata_store,
            edit_mode: EditMode::from(config.tui.edit_mode),
            tombstones: crate::components::tombstone::TombstoneSlots::new(),
            tombstone_undo_replay: false,
            config,
            config_path,
            theme,
            active_theme_name,
            palette_visible: false,
            palette_state: crate::components::palette::PaletteState::new(),
            transient_hint: None,
            legacy_archive_hint_shown: false,
            legacy_issue_close_hint_shown: false,
            dashboard_tab_empty_deprecation_shown: false,
            esc_chain: VecDeque::new(),
            landing,
            #[cfg(any(test, debug_assertions))]
            last_action: None,
        };

        // `App::default_license_state` is a local "no runtime seed" placeholder.
        // Real provider states, including inactive LicenseSeat states, still
        // hydrate the gate through the normal fail-closed path.
        if !is_placeholder_license_state(&app.license_state) {
            let initial_license_state = license_state_event_to_state(&app.license_state);
            app.feature_gate.update_state(&initial_license_state);
        }

        // Propagate the config-derived edit_mode to the dashboard's input bar.
        // `InputBar::new()` hardcodes EditMode::Emacs; without this sync, a
        // user with `tui.edit_mode = "vim"` would see Emacs on the dashboard
        // composer until they toggled. SessionDetail is None at boot and
        // receives the mode on instantiation, so it does not need syncing here.
        app.dashboard.set_edit_mode(app.edit_mode);
        app.dashboard
            .set_disable_paste_burst(app.config.tui.disable_paste_burst);

        // Apply landing-specific setup
        if let crate::landing::LandingDecision::SetupRequired = &app.landing {
            app.dashboard.set_agents_configured(false);
        }
        if app.metadata_store.is_read_only() {
            app.show_user_warning(READ_ONLY_STARTUP_WARNING.to_string());
        }
        app.sync_dashboard_workers();
        #[cfg(feature = "analytics")]
        {
            app.sync_live_cost_active_sessions();
            app.spawn_live_cost_refresh();
        }

        app.license_badge = license_badge_from_state(&app.license_state);
        app.flag_summary = compute_flag_summary();

        // Validate every agent entry. Fatal errors abort the agent (but we don't
        // crash the whole TUI — other agents may still work). Warnings are logged
        // and we continue.
        for entry in &app.config.agents.entries {
            match spur_acp::validate_agent_config(entry) {
                Ok(()) => {}
                Err(errors) => {
                    for e in errors {
                        if e.is_fatal() {
                            tracing::error!(agent = %entry.name, error = %e,
                                "agent config validation failed; this agent will not be usable");
                        } else {
                            tracing::warn!(agent = %entry.name, warning = %e,
                                "agent config validation warning");
                        }
                    }
                }
            }
        }

        if start_in_picker {
            if let Some(ref tx) = app.user_input_tx {
                let _ = tx.try_send(UserInput::ListSessions);
            }
        }

        app.sync_input_history();

        app
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn new_with_metadata_path_for_test(metadata_path: std::path::PathBuf) -> Self {
        Self::build_with_license_state_from_metadata_path(
            None,
            None,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            metadata_path,
            None,
        )
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn new_with_metadata_path_in_picker_for_test(metadata_path: std::path::PathBuf) -> Self {
        Self::build_with_license_state_from_metadata_path(
            None,
            Some(None),
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            metadata_path,
            None,
        )
    }

    /// Test-only accessor: borrow the current `SessionDetailView`.
    #[doc(hidden)]
    pub fn session_detail_for_test(
        &self,
    ) -> Option<&crate::views::session_detail::SessionDetailView> {
        self.session_detail.as_ref()
    }

    /// Test-only accessor: borrow the current licensing snapshot.
    #[doc(hidden)]
    pub fn license_state_for_test(&self) -> &LicenseStateEvent {
        &self.license_state
    }

    /// Test-only accessor: borrow the current licensing badge projection.
    #[doc(hidden)]
    pub fn license_badge_for_test(&self) -> Option<&LicenseBadge> {
        self.license_badge.as_ref()
    }

    pub(crate) fn feature_enabled_for_test(&self, key: spur_license::FeatureKey) -> bool {
        spur_license::require_feature(&self.feature_gate, key).is_ok()
    }

    /// Test-only accessor: borrow the first message waiting for trace seeding.
    #[doc(hidden)]
    pub fn pending_first_user_message_for_test(&self) -> Option<&str> {
        self.pending_first_user_message.as_deref()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn handle_undo_for_test(&mut self) {
        let _ = self.handle_undo();
    }

    #[cfg(any(test, debug_assertions))]
    pub fn tombstones_for_test(&mut self) -> &mut crate::components::tombstone::TombstoneSlots {
        &mut self.tombstones
    }

    #[cfg(any(test, debug_assertions))]
    fn ensure_user_input_capture_for_test(&mut self) {
        if self.user_input_rx_for_test.is_none() {
            let (tx, rx) = mpsc::channel::<UserInput>(16);
            self.user_input_tx = Some(tx);
            self.user_input_rx_for_test = Some(rx);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn add_pending_review_for_test(&mut self, executor_id: &str, attempt_n: u32) {
        use spur_acp::{ReviewKind, ReviewPayload, Role};

        self.ensure_user_input_capture_for_test();

        let executor = spur_core::ExecutorId(executor_id.to_string());
        if self.lineage.node(&executor).is_none() {
            self.lineage
                .apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
                    id: executor_id.into(),
                    parent_id: None,
                    session_id: SessionId(format!("session-{executor_id}")),
                    agent: "codex".into(),
                    role: Role::Executor,
                    task_spec: "test task".into(),
                }));
        }

        self.lineage
            .apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
                id: executor_id.into(),
                attempt_n,
                kind: ReviewKind::Completion,
                payload: ReviewPayload {
                    summary: "test pending review".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                    peer_influence: None,
                },
            }));
    }

    #[cfg(any(test, debug_assertions))]
    pub fn user_input_sent_for_test(&mut self) -> bool {
        self.user_input_sent_for_test_matching(None)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn user_input_sent_for_test_with_executor(&mut self, executor_id: &str) -> bool {
        self.user_input_sent_for_test_matching(Some(executor_id))
    }

    #[cfg(any(test, debug_assertions))]
    fn user_input_sent_for_test_matching(&mut self, expected_executor_id: Option<&str>) -> bool {
        let Some(rx) = self.user_input_rx_for_test.as_mut() else {
            return false;
        };

        let mut found = false;
        while let Ok(input) = rx.try_recv() {
            if let UserInput::SubmitReview { executor_id, .. } = input {
                let matches_expected = match expected_executor_id {
                    Some(expected) => executor_id == expected,
                    None => true,
                };
                found |= matches_expected;
            }
        }
        found
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_tracked_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        if self.issue_browser.is_none() {
            self.issue_browser = Some(IssueBrowserView::new());
        }
        if let Some(browser) = self.issue_browser.as_mut() {
            browser.set_issues_for_test(issues);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_edit_mode_for_test(&mut self, mode: EditMode) {
        self.edit_mode = mode;
        self.dashboard.set_edit_mode(mode);
        if let Some(detail) = self.session_detail.as_mut() {
            detail.set_edit_mode(mode);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn esc_chain_len_for_test(&self) -> usize {
        self.esc_chain.len()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn session_picker_for_test(&self) -> Option<&SessionPickerView> {
        self.session_picker.as_ref()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_session_picker_current_session_has_draft_for_test(
        &mut self,
        session_id: Option<String>,
    ) {
        if let Some(picker) = self.session_picker.as_mut() {
            picker.set_current_session_has_draft(session_id);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_metadata_store_for_test(&mut self, store: SessionMetadataStore) {
        self.metadata_store = store;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn metadata_store_for_test(&self) -> &SessionMetadataStore {
        &self.metadata_store
    }

    #[cfg(any(test, debug_assertions))]
    pub fn persist_metadata_for_test(&mut self, context: &'static str) -> bool {
        self.persist_metadata(context)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn handle_crossterm_event_for_test(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::Event;
        self.handle_crossterm_event(Event::Key(key));
    }

    #[cfg(any(test, debug_assertions))]
    pub fn dashboard_is_configured(&self) -> bool {
        self.dashboard.agents_configured()
    }

    /// Test-only accessor for the Dashboard view.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn dashboard_for_test(&self) -> &crate::views::dashboard::DashboardView {
        &self.dashboard
    }

    /// Test-only mutable accessor for the Dashboard view.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn dashboard_mut_for_test(&mut self) -> &mut crate::views::dashboard::DashboardView {
        &mut self.dashboard
    }

    #[cfg(any(test, debug_assertions))]
    pub fn open_dashboard_slash_picker_for_test(&mut self) {
        self.current_view = ViewId::Dashboard;
        self.dashboard.open_slash_picker_for_test();
    }

    /// Persist a draft to metadata. Callable both from the `Action::SaveDraft`
    /// handler (debounced tick path) and same-tick from exit-session boundaries
    /// via `force_flush_active_draft`.
    fn apply_save_draft(&mut self, session_id: String, draft: String) {
        let entry = self.metadata_store.entry_mut(&session_id);
        if entry.draft != draft {
            entry.draft = draft;
            self.persist_metadata("draft");
        }
    }

    /// Append a submitted message to the global input history (dedup + cap).
    fn push_input_history_entry(&mut self, entry: InputHistoryEntry) -> bool {
        if entry.snapshot.text.trim().is_empty() {
            return false;
        }
        let changed = {
            let hist = &mut self.metadata_store.metadata_mut().input_history;
            Self::merge_input_history_entry(hist, entry)
        };
        if changed {
            self.persist_metadata("input history");
            self.sync_input_history();
        }
        changed
    }

    fn merge_input_history_entry(
        hist: &mut Vec<InputHistoryEntry>,
        entry: InputHistoryEntry,
    ) -> bool {
        if entry.snapshot.text.trim().is_empty() {
            return false;
        }
        hist.retain(|existing| !existing.same_recall_state(&entry));
        hist.push(entry);
        if hist.len() > HISTORY_CAP {
            hist.remove(0);
        }
        true
    }

    /// Reseed all active InputBars with the current global history.
    fn sync_input_history(&mut self) {
        let hist = self.metadata_store.metadata().input_history.clone();
        self.dashboard.seed_input_history(hist.clone());
        if let Some(ref mut detail) = self.session_detail {
            detail.seed_input_history(hist);
        }
    }

    /// Synchronously flush the active SessionDetailView's unsent InputBar text
    /// to metadata, bypassing the 500ms debounce. Call at user-intent "exit
    /// session" boundaries (opening the picker, quit-confirm proceed, brain
    /// respawn for a different session id) so metadata reflects the latest
    /// on-screen text before anything reads it. No-op when no detail is active
    /// or the draft is unchanged since the last persist.
    fn force_flush_active_draft(&mut self) {
        let Some(detail) = self.session_detail.as_mut() else {
            return;
        };
        if let Some(Action::SaveDraft { session_id, draft }) = detail.force_save_draft() {
            self.apply_save_draft(session_id, draft);
        }
    }

    /// Returns `Some(sid)` if the currently-active session has a non-empty
    /// persisted draft; else `None`. Used by the picker to decide whether to
    /// show the switch-safety confirm banner.
    fn compute_draft_session(&self) -> Option<String> {
        let detail = self.session_detail.as_ref()?;
        let sid = detail.session_id().0.clone();
        let has = self
            .metadata_store
            .entry(&sid)
            .map(|e| !e.draft.is_empty())
            .unwrap_or(false);
        if has {
            Some(sid)
        } else {
            None
        }
    }

    /// Push the current metadata snapshot AND current-draft awareness into the
    /// picker if one exists. Call from any action that mutates metadata.
    fn refresh_picker_metadata(&mut self) {
        let draft = self.compute_draft_session();
        let current = self
            .session_detail
            .as_ref()
            .map(|d| d.session_id().0.clone());
        if let Some(ref mut picker) = self.session_picker {
            picker.set_metadata(self.metadata_store.metadata().clone());
            picker.set_current_session_has_draft(draft);
            picker.set_current_session_id(current);
        }
    }

    /// Push current brain status to both views' InputBars.
    fn sync_brain_status(&mut self) {
        let session_attached = self
            .session_detail
            .as_ref()
            .is_some_and(|detail| !detail.is_cleared());
        let status_str = match &self.brain_status {
            BrainStatus::Idle => "idle",
            BrainStatus::Connecting => "connecting",
            BrainStatus::Connected => "connected",
            BrainStatus::Thinking => "thinking",
            BrainStatus::Streaming => "streaming",
            BrainStatus::Ready => "ready",
            BrainStatus::Error(_) => "error",
        };

        self.dashboard
            .set_brain_status(self.brain_name.as_deref(), status_str, session_attached);

        if let Some(ref mut detail) = self.session_detail {
            detail.set_brain_status(status_str);
        }
    }

    /// Read-only access to per-executor `ReactTrace` instances.
    pub fn worker_streams(&self) -> &crate::worker_streams::WorkerStreams {
        &self.worker_streams
    }

    /// Mutable access to per-executor `ReactTrace` instances.
    pub fn worker_streams_mut(&mut self) -> &mut crate::worker_streams::WorkerStreams {
        &mut self.worker_streams
    }

    pub fn plan_projection(&self) -> &PlanProjectionStore {
        &self.plan_projection
    }

    pub fn synopsis(&self) -> &SessionSynopsisProjection {
        &self.synopsis
    }

    #[cfg(any(test, debug_assertions))]
    pub fn age_issue_browser_prefetch_for_test(&mut self, age: Duration) {
        if let Some(view) = self.issue_browser.as_mut() {
            view.age_pending_prefetch_for_test(age);
        }
    }

    /// Tick the active view (for animations, batched text flush, etc.).
    pub fn tick(&mut self) {
        let now = Instant::now();
        // Drive tombstone expiry. Expired reversible tombstones are silently
        // dropped; expired QueuedRemote tombstones dispatch through App.
        let expired_queued = self.tombstones.tick(now);
        for action in expired_queued {
            self.process_action(action);
        }
        self.tick_transient_hint(now);

        #[cfg(feature = "markdown")]
        {
            while let Ok(action) = self.mermaid_rx.try_recv() {
                self.process_action(action);
            }
        }

        #[cfg(feature = "analytics")]
        self.drain_insights_init();

        if let Some((_, deadline)) = &self.pending_permission {
            if now >= *deadline {
                self.pending_permission.take(); // drops reply_tx → auto-deny
                self.clear_pending_permission_trace();
                self.dirty = true;
            }
        }

        // Only mark dirty for ticks when there are active agents (spinners animating)
        // or text batches to flush.
        match self.current_view {
            ViewId::Dashboard => {
                self.dashboard.tick();
                let flushed_batch_or_paste = self.dashboard.tick_and_report_flush();
                // Mark dirty when executors are actively running (spinners animate)
                use spur_core::LifecycleState;
                let has_active = self.lineage.nodes().any(|n| {
                    matches!(
                        n.phase,
                        LifecycleState::Running
                            | LifecycleState::Spawning
                            | LifecycleState::Resuming
                    )
                });
                if has_active
                    || flushed_batch_or_paste
                    || self.dashboard.input_bar_has_active_animation()
                {
                    self.dirty = true;
                }
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
                    self.dirty = true; // session detail always has activity
                }
                #[cfg(feature = "markdown")]
                {
                    let pending: Vec<Action> = self
                        .session_detail
                        .as_mut()
                        .map(|d| d.take_pending_actions())
                        .unwrap_or_default();
                    for action in pending {
                        self.process_action(action);
                    }
                }
                // Debounced draft persistence — fires ~500ms after the last
                // InputBar keystroke, then no-ops until the next change.
                let draft_action = self
                    .session_detail
                    .as_mut()
                    .and_then(|d| d.draft_save_action());
                if let Some(action) = draft_action {
                    self.process_action(action);
                }
            }
            ViewId::SessionPicker => {
                if let Some(p) = self.session_picker.as_mut() {
                    p.tick()
                }
            }
            ViewId::PlanInspector(_) => {
                if let Some(view) = self.plan_inspector.as_mut() {
                    view.tick();
                }
            }
            ViewId::PlanBrowser => {
                if let Some(view) = self.plan_browser.as_mut() {
                    view.tick();
                }
            }
            ViewId::IssueBrowser => {
                let pending = if let Some(view) = self.issue_browser.as_mut() {
                    view.tick();
                    view.take_pending_action()
                } else {
                    None
                };
                if let Some(action) = pending {
                    self.process_action(action);
                }
            }
            ViewId::Insights => {}
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => {
                // The underlying session detail continues receiving
                // AgentMessageChunks while the overlay is open. Tick it so
                // debounced flushes and fence dispatches don't stall.
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
                    let pending = detail.take_pending_actions();
                    for action in pending {
                        self.process_action(action);
                    }
                    self.dirty = true;
                }
            }
        }

        // Advance spinner frames on all per-executor traces every tick,
        // regardless of which view is focused.  Keeps traces ready when the
        // user navigates to them (risk-register PR10: O(entries) per trace).
        self.worker_streams.tick_all();
    }

    /// Render the active view, then overlay help if visible.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let (banner_area, view_area) = if self.user_warning.is_some() {
            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Min(0),
                ])
                .split(area);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, area)
        };

        // Construct the shared context once per frame.
        let transient_hint_text = self.transient_hint.as_ref().map(|hint| hint.text.clone());
        let transient_hint_override = transient_hint_text.as_deref().map(HintOverride::from_full);

        let ctx = crate::views::ViewContext {
            lineage: &self.lineage,
            plan_projection: &self.plan_projection,
            synopsis: &self.synopsis,
            brain_status: &self.brain_status,
            license_badge: self.license_badge.as_ref(),
            flag_summary: self.flag_summary,
            tombstone: self.tombstones.peek(self.current_view()),
            transient_hint_override,
            theme: &self.theme,
        };

        #[cfg(feature = "analytics")]
        crate::components::status_bar::set_via_analytics_visible(
            self.via_analytics_visible_for_current_view(),
        );

        match self.current_view.clone() {
            ViewId::Dashboard => {
                self.dashboard
                    .render_with_lineage(frame, view_area, &mut self.worker_streams, &ctx)
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.render(frame, view_area, &ctx);
                }
            }
            ViewId::SessionPicker => {
                if let Some(ref mut p) = self.session_picker {
                    p.render(frame, view_area, &ctx);
                }
            }
            ViewId::PlanInspector(_) => {
                if let Some(ref mut view) = self.plan_inspector {
                    view.render(frame, view_area, &ctx);
                }
            }
            ViewId::PlanBrowser => {
                if let Some(ref mut view) = self.plan_browser {
                    view.render(frame, view_area, &ctx);
                }
            }
            ViewId::IssueBrowser => {
                if let Some(ref mut view) = self.issue_browser {
                    view.render(frame, view_area, &ctx);
                }
            }
            #[cfg(feature = "analytics")]
            ViewId::Insights => {
                if let Some(ref mut view) = self.insights_view {
                    view.render(frame, view_area, &ctx);
                } else if let Some(state) = self.insights_init.as_ref() {
                    analytics::render_insights_init_placeholder(frame, view_area, state.started_at);
                }
            }
            #[cfg(not(feature = "analytics"))]
            ViewId::Insights => {}
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(ref session) => {
                let session_matches = self
                    .session_detail
                    .as_ref()
                    .map(|d| d.session_id().0 == session.0)
                    .unwrap_or(false);
                if session_matches {
                    if let Some(detail) = self.session_detail.as_mut() {
                        let entries: Vec<(
                            crate::components::mermaid::MermaidId,
                            &crate::components::mermaid::MermaidState,
                        )> = detail
                            .mermaid_registry
                            .iter()
                            .map(|(k, v)| (*k, v))
                            .collect();
                        if let Some(viewer) = self.mermaid_viewer.as_mut() {
                            viewer.set_available(&entries);
                            render_mermaid_overlay(frame, view_area, viewer, detail);
                        }
                    }
                }
            }
        }

        if self.help_visible {
            #[cfg(feature = "markdown")]
            let mermaid_enabled = self.mermaid_picker.is_some();
            #[cfg(not(feature = "markdown"))]
            let mermaid_enabled = false;
            HelpOverlay::render(frame, view_area, mermaid_enabled, true);
        }

        if self.quit_confirm_visible {
            QuitConfirmDialog::render(frame, view_area, self.brain_name.as_deref());
        }

        if let Some(state) = &self.collision_modal {
            CollisionModal::render(frame, view_area, &state.acp_id, &state.holder);
        }

        if self.palette_visible {
            let overlay =
                crate::components::palette_overlay::PaletteOverlay::new(&self.palette_state)
                    .with_session_active(self.session_detail.is_some());
            frame.render_widget(overlay, view_area);
        }

        // Plan C Tier 2 — upgrade modal renders LAST among overlays so it
        // visually preempts every informational overlay (matches the event-
        // priority placement between collision_modal and help_visible).
        // Suppress the upgrade modal whenever a higher-precedence modal is
        // up (quit_confirm or collision) so the visual matches input
        // precedence: quit_confirm > collision > upgrade in BOTH dimensions.
        if self.should_render_upgrade_modal() {
            if let Some(state) = &self.upgrade_modal {
                upgrade_modal::render(frame, view_area, state);
            }
        }

        if let (Some(area), Some(message)) = (banner_area, self.user_warning.as_deref()) {
            render_user_warning(frame, area, message);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        #[cfg(feature = "analytics")]
        if let Some(handle) = self.live_cost_handle.take() {
            handle.abort();
        }
    }
}

fn is_quit_chord(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
}

// ─── Main TUI entry point ──────────────────────────────────────────────

/// Run the TUI dashboard, consuming events from the broadcast receiver.
pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
) -> anyhow::Result<()> {
    run_tui_with_license(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker.then_some(None),
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        App::default_license_state(PLACEHOLDER_STATUS_TEXT),
        crate::landing::LandingDecision::ShowDashboard,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tui_with_license(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker_with_preselect: Option<Option<String>>,
    config: std::sync::Arc<spur_acp::SpurConfig>,
    license_state: LicenseStateEvent,
    landing: crate::landing::LandingDecision,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::build_with_license_state(
        user_input_tx,
        start_in_picker_with_preselect,
        config.clone(),
        license_state,
        landing,
        config_path,
    );
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

    // === bd-1vnk: rehydrate projections from prior NDJSON before drain begins ===
    let replay_cfg = spur_core::event_replay::ReplayConfig {
        replay_horizon: std::time::Duration::from_secs(config.log.event_replay_horizon_secs),
        ..Default::default()
    };
    match spur_core::event_replay::replay_events(&replay_cfg, |ev| {
        app.lineage.apply(ev);
        app.plan_projection.apply(ev);
        app.synopsis.apply(ev);
    }) {
        Ok(stats) => tracing::info!(
            target: "spur.metrics.event_replay",
            files = stats.files_read,
            skipped_pid = stats.files_skipped_pid,
            applied = stats.events_applied,
            horizon_skipped = stats.events_skipped_horizon,
            malformed = stats.malformed_lines,
            elapsed_ms = stats.elapsed.as_millis() as u64,
        ),
        Err(e) => tracing::error!(
            error = %e,
            "event replay failed; starting with empty projections"
        ),
    }
    // ============================================================================

    // Bridge OS termination signals into the event loop so SIGINT/SIGTERM/SIGHUP/SIGQUIT
    // run the same teardown as Ctrl-C/Ctrl-Q (raw mode off → alt screen exit →
    // function returns → caller drops Orchestrator). SIGKILL is uncatchable;
    // the on-startup orphan sweep is the safety net for that case.
    //
    // mpsc(1) coalesces duplicate signals via try_send: Err(Full(_)) means a
    // shutdown is already pending, which is exactly what we want.
    let (_shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, Signal, SignalKind};
        // Graceful fallback on signal-registration failure: log + skip the
        // handler instead of panicking. A panic here would exit AFTER raw
        // mode + alt screen are entered, leaving the user with a corrupt
        // terminal (no echo, stuck alt screen). Rare in practice but
        // possible in sandboxed / fork-restricted / signalfd-disabled
        // environments. (bd-2j5e.5)
        fn install(kind: SignalKind, label: &str) -> Option<Signal> {
            match signal(kind) {
                Ok(s) => Some(s),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        signal = %label,
                        "failed to install signal handler; this signal will not gracefully shut down the TUI"
                    );
                    None
                }
            }
        }
        let mut sigterm = install(SignalKind::terminate(), "SIGTERM");
        let mut sighup = install(SignalKind::hangup(), "SIGHUP");
        let mut sigquit = install(SignalKind::quit(), "SIGQUIT");
        let tx = _shutdown_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "SIGINT handler failed");
                            return;
                        }
                        let _ = tx.try_send(());
                    }
                    _ = async {
                        match sigterm.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                    _ = async {
                        match sighup.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                    _ = async {
                        match sigquit.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                }
            }
        });
    }

    loop {
        // Count how many events feed into each render. H1' detection.
        let mut spur_drained: u32 = 0;
        let mut crossterm_drained: u32 = 0;

        // Phase 1: Wait for at least one event (async yield point).
        tokio::select! {
            Some(Ok(crossterm_event)) = event_stream.next() => {
                crossterm_drained += 1;
                app.handle_crossterm_event(crossterm_event);
            }
            result = event_rx.recv() => {
                match result {
                    Ok(spur_event) => {
                        spur_drained += 1;
                        app.handle_spur_event(spur_event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            streaming_probe = true,
                            site = "E_broadcast_lag",
                            lagged_n = n,
                            source = file!(),
                            line = line!(),
                            "TUI broadcast receiver lagged — events dropped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        app.should_quit = true;
                    }
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
            Some(perm) = async {
                match perm_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.handle_permission_request(perm);
            }
            _ = shutdown_rx.recv() => {
                // SIGINT / SIGTERM / SIGHUP / SIGQUIT: take the same path as a confirmed
                // Ctrl-Q. confirm_quit() flushes drafts + sets should_quit; the
                // existing loop break runs the shared tui::teardown and the
                // function returns so the caller's host.shutdown().await issues
                // killpg and unregisters the pgid registry. Bypassing Drop here
                // (e.g., via std::process::exit) defeats the orphan-reaping
                // safety guarantees on catchable signals.
                app.confirm_quit();
            }
        }

        // Phase 2: Drain all remaining crossterm events (non-blocking).
        // This collapses bursts of mouse scroll events into one render pass.
        while let Ok(Some(Ok(ev))) = timeout(Duration::ZERO, event_stream.next()).await {
            crossterm_drained += 1;
            app.handle_crossterm_event(ev);
        }

        // Phase 3: Drain remaining spur events (non-blocking), capped per frame.
        //
        // S1.c (H1') — cap at DRAIN_CAP_PER_FRAME so bursts of streaming chunks
        // don't collapse into a single paint. Leftover events drain on the next
        // iteration; no event is lost, just deferred by one frame. `Lagged`
        // counts toward the cap so a subscriber that's badly behind still makes
        // progress instead of spinning on drop notifications.
        const DRAIN_CAP_PER_FRAME: u32 = 8;
        let mut drained_this_phase: u32 = 0;
        while drained_this_phase < DRAIN_CAP_PER_FRAME {
            match event_rx.try_recv() {
                Ok(spur_event) => {
                    spur_drained += 1;
                    drained_this_phase += 1;
                    app.handle_spur_event(spur_event);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!(
                        streaming_probe = true,
                        site = "E_broadcast_lag",
                        lagged_n = n,
                        source = file!(),
                        line = line!(),
                        "TUI broadcast receiver lagged (drain phase) — events dropped"
                    );
                    drained_this_phase += 1;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // Phase 4: Single render pass.
        if app.dirty {
            if spur_drained > 0 || crossterm_drained > 0 {
                tracing::debug!(
                    streaming_probe = true,
                    site = "F_frame_drain",
                    spur_drained = spur_drained,
                    crossterm_drained = crossterm_drained,
                    "rendering frame"
                );
            }
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal first so the user regains control immediately,
    // even if the best-effort analytics checkpoint hits its 2s timeout.
    tui::teardown(&mut terminal)?;
    app.shutdown_analytics().await;
    Ok(())
}

pub async fn run_tui_with_config(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
    config: std::sync::Arc<spur_acp::SpurConfig>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    run_tui_with_license(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker.then_some(None),
        config,
        App::default_license_state(PLACEHOLDER_STATUS_TEXT),
        crate::landing::LandingDecision::ShowDashboard,
        config_path,
    )
    .await
}

// ─── Free helpers ──────────────────────────────────────────────────────

#[cfg(feature = "markdown")]
fn render_mermaid_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    viewer: &mut crate::views::mermaid_viewer::MermaidViewerView,
    detail: &mut crate::views::session_detail::SessionDetailView,
) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };
    use ratatui_image::{Resize, StatefulImage};

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Mermaid Viewer ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    let drew = (|| {
        let id = viewer.focused?;
        let picker = detail.render_picker.as_ref()?;
        let (image, image_generation) = match detail.mermaid_registry.get(&id)? {
            crate::components::mermaid::MermaidState::Ready {
                image,
                image_generation,
                ..
            } => (image.clone(), *image_generation),
            _ => return None,
        };
        let proto = detail
            .image_cache
            .overlay_protocol_mut(id, &image, image_generation, picker);
        let widget = StatefulImage::default().resize(Resize::Fit(None));
        frame.render_stateful_widget(widget, chunks[1], proto);
        Some(())
    })()
    .is_some();

    if !drew {
        frame.render_widget(
            Paragraph::new(
                "No diagram available yet. Wait for render to complete, or press q/Esc to return.",
            )
            .style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
}

/// Human-friendly relative time for the resume banner ("5m ago", "2h ago").
/// Returns "recently" if the input is missing or unparseable.
fn humanize_since(iso: Option<&str>) -> String {
    let Some(iso) = iso else {
        return "recently".into();
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return "recently".into();
    };
    let secs = chrono::Utc::now().signed_duration_since(dt).num_seconds();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(any(test, debug_assertions))]
impl App {
    /// Minimal `App` for unit tests. Avoids disk I/O from
    /// `SessionMetadataStore::load`.
    pub fn new_for_tests() -> Self {
        App::new(None, false)
    }
}

#[cfg(test)]
mod issue_browser_navigation_tests {
    use super::*;

    fn issue_summary(id: &str, title: &str) -> spur_acp::IssueSummaryEvent {
        spur_acp::IssueSummaryEvent {
            id: id.into(),
            source: "beads".into(),
            title: title.into(),
            status: "open".into(),
            labels: Vec::new(),
            priority: Some(1),
            issue_type: Some("bug".into()),
            assignee: None,
        }
    }

    #[test]
    fn navigate_to_issue_browser_seeds_from_dashboard_cache_and_refreshes_once() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.handle_spur_event(SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![issue_summary("bd-1809", "IssueBrowser starts populated")],
        }));

        app.process_action(Action::NavigateTo(ViewId::IssueBrowser));

        let tracked = app
            .issue_browser
            .as_ref()
            .expect("navigation should lazily create IssueBrowser")
            .tracked_issues();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].id, "bd-1809");
        assert_eq!(tracked[0].title, "IssueBrowser starts populated");

        match rx.try_recv() {
            Ok(UserInput::RefreshIssues) => {}
            Ok(_) => panic!("expected first IssueBrowser navigation to request RefreshIssues"),
            Err(err) => panic!("expected RefreshIssues after first navigation, got {err}"),
        }

        app.process_action(Action::NavigateTo(ViewId::Dashboard));
        app.process_action(Action::NavigateTo(ViewId::IssueBrowser));

        assert!(
            rx.try_recv().is_err(),
            "existing IssueBrowser should not request another refresh on navigation"
        );
    }
}

#[cfg(test)]
mod plan_browser_navigation_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{PlanLifecycleEvent, PlanOwnerStateEvent, PlanSummaryEvent};

    fn plan_summary(plan_id: &str, owner_state: PlanOwnerStateEvent) -> PlanSummaryEvent {
        PlanSummaryEvent {
            plan_id: plan_id.into(),
            epic_id: format!("bd-{plan_id}"),
            title: format!("Plan {plan_id}"),
            source_body_preview: None,
            owner_state,
            lifecycle: PlanLifecycleEvent::Pending,
            counts: None,
            updated_at: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn wrap(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    #[test]
    fn navigate_to_plan_browser_lazily_creates_and_refreshes_once() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(app.current_view(), &ViewId::PlanBrowser);
        assert!(
            app.plan_browser.is_some(),
            "navigation should lazily create PlanBrowser"
        );
        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected RefreshPlans, got different user input"),
            Err(err) => panic!("expected RefreshPlans after first navigation, got {err}"),
        }

        app.process_action(Action::NavigateTo(ViewId::Dashboard));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert!(
            rx.try_recv().is_err(),
            "existing PlanBrowser should not request another refresh on navigation"
        );
    }

    #[test]
    fn navigate_to_plan_browser_without_session_blocks_with_hint() {
        // Inc 1 (bd-d587.1): without an active brain session, opening PlanBrowser
        // would yield a list where no row can ever classify as Mine. We block-with-hint
        // instead of opening an empty browser.
        let mut app = App::new_for_tests();

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(
            app.current_view(),
            &ViewId::Dashboard,
            "navigation must be refused when no session is active"
        );
        assert!(
            app.plan_browser.is_none(),
            "PlanBrowser must not be created when navigation is refused"
        );
    }

    #[test]
    fn resume_plan_action_sends_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::ResumePlan {
            plan_id: "plan-42".into(),
        });

        match rx.try_recv() {
            Ok(UserInput::ResumePlan { plan_id }) => assert_eq!(plan_id, "plan-42"),
            Ok(_) => panic!("expected ResumePlan, got different user input"),
            Err(err) => panic!("expected ResumePlan user input, got {err}"),
        }
    }

    #[test]
    fn claim_plan_action_sends_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::ClaimPlan {
            plan_id: "plan-42".into(),
        });

        match rx.try_recv() {
            Ok(UserInput::ClaimPlan { plan_id }) => assert_eq!(plan_id, "plan-42"),
            Ok(_) => panic!("expected ClaimPlan, got different user input"),
            Err(err) => panic!("expected ClaimPlan user input, got {err}"),
        }
    }

    #[test]
    fn navigating_existing_plan_browser_updates_current_session() {
        // Inc 1 (bd-d587.1): seed an initial brain so the first navigation succeeds,
        // then assert that re-navigating after a session swap updates current_session
        // on the already-created PlanBrowser.
        let mut app = App::new_for_tests();
        let first = SessionId("brain-1".into());
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: first.clone(),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        assert_eq!(
            app.plan_browser
                .as_ref()
                .expect("PlanBrowser should exist")
                .current_session_for_test(),
            &first,
        );

        let second = SessionId("brain-2".into());
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: second.clone(),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(
            app.plan_browser
                .as_ref()
                .expect("PlanBrowser should still exist")
                .current_session_for_test(),
            &second
        );
    }

    #[test]
    fn open_issue_in_backlog_navigates_and_fetches_detail() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::OpenIssueInBacklog {
            id: "bd-plan-1".into(),
        });

        assert_eq!(app.current_view(), &ViewId::IssueBrowser);
        assert!(
            app.issue_browser.is_some(),
            "OpenIssueInBacklog should create IssueBrowser"
        );
        match rx.try_recv() {
            Ok(UserInput::GetIssueDetail { id }) => assert_eq!(id, "bd-plan-1"),
            Ok(_) => panic!("expected GetIssueDetail for backlog epic, got different user input"),
            Err(err) => panic!("expected GetIssueDetail for backlog epic, got {err}"),
        }
    }

    #[test]
    fn plan_browser_spur_events_route_to_view() {
        let mut app = App::new_for_tests();
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        app.handle_spur_event(SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: vec![plan_summary("plan-1", PlanOwnerStateEvent::Unowned)],
            warnings: Vec::new(),
        }));

        let plans = app
            .plan_browser
            .as_ref()
            .expect("PlanBrowser should exist")
            .plans();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].plan_id, "plan-1");
    }

    #[test]
    fn plan_browser_keys_bridge_refresh_claim_and_start_to_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected initial RefreshPlans, got different user input"),
            Err(err) => panic!("expected initial RefreshPlans, got {err}"),
        }
        app.handle_spur_event(SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: vec![plan_summary("plan-1", PlanOwnerStateEvent::Unowned)],
            warnings: Vec::new(),
        }));

        app.handle_crossterm_event_for_test(key(KeyCode::Char('r')));
        app.handle_crossterm_event_for_test(key(KeyCode::Char('c')));
        app.handle_crossterm_event_for_test(key(KeyCode::Enter));

        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected RefreshPlans from r key, got different user input"),
            Err(err) => panic!("expected RefreshPlans from r key, got {err}"),
        }
        match rx.try_recv() {
            Ok(UserInput::ClaimPlan { plan_id }) => assert_eq!(plan_id, "plan-1"),
            Ok(_) => panic!("expected ClaimPlan from c confirm, got different user input"),
            Err(err) => panic!("expected ClaimPlan from c confirm, got {err}"),
        }
    }
}

/// Inc 2 (bd-d587.2): unit tests for the view_history stack semantics.
/// Drives `navigate_to` / `navigate_back` directly (not via Action arms)
/// so the invariants are tested in isolation from action-routing logic.
#[cfg(test)]
mod view_history_tests {
    use super::*;
    use spur_acp::SessionId;

    fn seed_session(app: &mut App, sid: &str) {
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "test-brain".into(),
            session: SessionId(sid.into()),
        }));
    }

    #[test]
    fn navigate_to_pushes_leaving_view_then_back_pops_it() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        // BrainSpawned auto-navigated us into SessionDetail. Stack should be [Dashboard].
        assert_eq!(app.view_history, vec![ViewId::Dashboard]);

        app.navigate_to(ViewId::IssueBrowser);
        assert_eq!(app.current_view, ViewId::IssueBrowser);
        assert_eq!(
            app.view_history,
            vec![
                ViewId::Dashboard,
                ViewId::SessionDetail(SessionId("brain-1".into()))
            ],
        );

        app.navigate_back();
        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into()))
        );
        assert_eq!(app.view_history, vec![ViewId::Dashboard]);

        app.navigate_back();
        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(app.view_history.is_empty());
    }

    #[test]
    fn navigate_to_dashboard_clears_history() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        app.navigate_to(ViewId::IssueBrowser);
        app.navigate_to(ViewId::SessionPicker);
        assert!(app.view_history.len() >= 2);

        app.navigate_to(ViewId::Dashboard);

        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(
            app.view_history.is_empty(),
            "Dashboard is canonical root and must clear history"
        );
    }

    #[test]
    fn navigate_to_same_view_is_no_op() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        let history_before = app.view_history.clone();

        app.navigate_to(ViewId::SessionDetail(SessionId("brain-1".into())));

        assert_eq!(
            app.view_history, history_before,
            "navigate_to(current_view) must not push or mutate history"
        );
    }

    #[test]
    fn push_history_skips_duplicate_top() {
        let mut app = App::new_for_tests();
        app.view_history.push(ViewId::IssueBrowser);
        app.push_history(ViewId::IssueBrowser);
        assert_eq!(app.view_history, vec![ViewId::IssueBrowser]);
    }

    #[test]
    fn push_history_caps_at_max_evicting_oldest() {
        let mut app = App::new_for_tests();
        // Pre-fill exactly to the cap with a non-Dashboard, non-current view.
        for _ in 0..NAV_HISTORY_MAX {
            app.view_history.push(ViewId::IssueBrowser);
            // Defeat the no-dup-top guard by alternating — easier to use raw push for this test.
        }
        // Force overflow via the public API.
        app.push_history(ViewId::SessionPicker);

        assert_eq!(app.view_history.len(), NAV_HISTORY_MAX);
        assert_eq!(
            app.view_history.last(),
            Some(&ViewId::SessionPicker),
            "newest entry must remain at the top"
        );
    }

    #[test]
    fn navigate_back_from_dashboard_with_active_session_falls_back_to_session_detail() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        // Land back on Dashboard with empty history.
        app.navigate_to(ViewId::Dashboard);
        assert!(app.view_history.is_empty());
        assert_eq!(app.current_view, ViewId::Dashboard);

        app.navigate_back();

        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into())),
            "Dashboard back-with-empty-history returns to active session detail"
        );
    }

    #[test]
    fn navigate_back_from_dashboard_with_no_session_is_no_op() {
        let mut app = App::new_for_tests();
        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(app.view_history.is_empty());
        assert!(app.session_detail.is_none());

        app.navigate_back();

        assert_eq!(
            app.current_view,
            ViewId::Dashboard,
            "no session + empty history must not move the user anywhere"
        );
    }

    #[test]
    fn navigate_back_nulls_plan_inspector_overlay_state() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        app.process_action(Action::NavigateTo(ViewId::PlanInspector(SessionId(
            "brain-1".into(),
        ))));
        assert!(app.plan_inspector.is_some());

        app.process_action(Action::NavigateBack);

        assert!(
            app.plan_inspector.is_none(),
            "leaving PlanInspector via navigate_back must null the overlay state"
        );
    }

    #[test]
    fn end_to_end_dashboard_to_sprints_to_issue_browser_back_chain() {
        // Reproduces the user-reported flow: Dashboard \u2192 SessionDetail \u2192 PlanBrowser
        // \u2192 (e for view-epic) IssueBrowser \u2192 Esc must land back at PlanBrowser
        // (not Dashboard, which was the pre-Inc-2 bug).
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        assert_eq!(app.current_view, ViewId::PlanBrowser);

        app.process_action(Action::OpenIssueInBacklog {
            id: "bd-epic-1".into(),
        });
        assert_eq!(app.current_view, ViewId::IssueBrowser);

        // Drive a real Esc keystroke through the crossterm path so the
        // IssueBrowser view's own handler is exercised (not just the action
        // it should produce). This is the regression hook: previously the
        // view returned NavigateTo(Dashboard) here, which silently bypassed
        // view_history and skipped past PlanBrowser entirely.
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.current_view,
            ViewId::PlanBrowser,
            "Esc from IssueBrowser must return to PlanBrowser, not Dashboard"
        );

        app.process_action(Action::NavigateBack);
        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into())),
            "Esc from PlanBrowser must return to SessionDetail"
        );
    }
}

#[cfg(test)]
mod license_gate_refresh_tests {
    use super::*;

    fn pro_license_state_event() -> LicenseStateEvent {
        LicenseStateEvent {
            status: LicenseStatusEvent::Active,
            subject_kind: LicenseSubjectKind::User,
            plan: EventLicensePlan::Pro,
            features: spur_license::policy::PolicyResolver::embedded()
                .tier_features("pro")
                .expect("embedded policy must define pro tier features"),
            expires_at: None,
            binding_mode: LicenseBindingMode::NodeLocked,
            offline_ok: true,
            status_text: "Pro license active".into(),
        }
    }

    fn assert_pro_cost_tracking_enabled(app: &App) {
        spur_license::require_feature(
            &app.feature_gate,
            spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
        )
        .expect("Pro cost tracking should be enabled");
    }

    #[test]
    fn license_update_refreshes_feature_gate_snapshot() {
        let mut app = App::new_for_tests();
        assert!(spur_license::require_feature(
            &app.feature_gate,
            spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
        )
        .is_err());

        app.handle_spur_event(SpurEvent::now(SpurEventBody::LicenseUpdated {
            state: pro_license_state_event(),
        }));

        assert_pro_cost_tracking_enabled(&app);
    }

    #[test]
    fn seeded_license_state_hydrates_feature_gate_snapshot() {
        let app = App::new_with_license(
            None,
            false,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            pro_license_state_event(),
            crate::landing::LandingDecision::ShowDashboard,
            None,
        );

        assert_pro_cost_tracking_enabled(&app);
    }
}

#[cfg(all(test, feature = "analytics"))]
mod insights_navigation_tests {
    use super::*;

    /// `InsightsView::new` spawns a tokio refresh task, so these tests need
    /// an active runtime. `ensure_insights_engine_and_view` would otherwise
    /// open a real `~/.spur/cache/cost.duckdb`; we pre-seed the App with an
    /// in-memory `AsyncEngine` so the constructor takes its fast path.
    fn boot_test_app() -> (tokio::runtime::Runtime, App) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut app = {
            let _guard = rt.enter();
            App::new_for_tests()
        };
        let in_memory = spur_context::AnalyticsEngine::open_in_memory().unwrap();
        in_memory.initialize().unwrap();
        in_memory.create_agent_views().unwrap();
        app.analytics_engine = Some(spur_context::AsyncEngine::new(in_memory));
        (rt, app)
    }

    #[test]
    fn alt_a_opens_insights_view() {
        let (rt, mut app) = boot_test_app();
        let _guard = rt.enter();

        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));

        assert_eq!(app.current_view(), &ViewId::Insights);
    }

    /// macOS Terminal/iTerm with default "Use Option as Meta key" OFF emits
    /// the Unicode char `å` for Option+A. The global Insights bypass must
    /// trigger AFTER `normalize_macos_option` runs at the app entry point.
    #[test]
    fn macos_option_a_opens_insights_view() {
        use crossterm::event::Event;

        let (rt, mut app) = boot_test_app();
        let _guard = rt.enter();

        app.handle_crossterm_event(Event::Key(KeyEvent::new(
            KeyCode::Char('å'),
            KeyModifiers::NONE,
        )));

        assert_eq!(app.current_view(), &ViewId::Insights);
    }
}

#[cfg(test)]
mod worker_stream_routing_tests {
    use super::*;
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use spur_acp::SessionId;
    use spur_acp::{ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent};

    fn msg_update(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        ))))
    }

    fn test_app() -> App {
        App::new_for_tests()
    }

    fn wrap_event(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    #[test]
    fn worker_notification_populates_per_executor_trace() {
        let mut app = test_app();
        // Seed lineage with the executor first — routing drops orphan events.
        app.lineage
            .apply(&wrap_event(SpurEventBody::ExecutorSpawned {
                id: "exec-42".into(),
                parent_id: None,
                session_id: SessionId("abc".into()),
                agent: "claude".into(),
                role: spur_acp::Role::Executor,
                task_spec: String::new(),
            }));
        let notif = Box::new(SessionNotification::new(
            "abc",
            msg_update("hello from worker"),
        ));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "exec-42".into(),
            notification: notif,
        }));
        let trace = app
            .worker_streams()
            .get("exec-42")
            .expect("trace for spawned executor");
        assert_eq!(trace.entry_count(), 1);
    }

    #[tokio::test]
    async fn run_tui_replay_populates_synopsis_from_prior_ndjson() {
        use std::io::Write;

        // spur-tui does not depend on serial_test; this process-wide CWD
        // mutation can flake if another parallel test depends on CWD.
        // Share `theme::runtime::test_support::TEST_LOCK` so that any
        // `with_isolated_dirs` caller (e.g. theme threading + /theme
        // command tests) is serialized against this cwd swap.
        let _lock = crate::theme::runtime::test_support::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        std::fs::create_dir_all(".spur/events").unwrap();

        // Write a fixture NDJSON file from a "prior" PID.
        let path = std::path::PathBuf::from(".spur/events/100-1000-0.ndjson");
        let mut f = std::fs::File::create(&path).unwrap();
        let ev = wrap_event(SpurEventBody::AgentNotification {
            session: spur_acp::SessionId("test-sess".into()),
            notification: Box::new(agent_client_protocol::schema::SessionNotification::new(
                agent_client_protocol::schema::SessionId::new("test-sess"),
                agent_client_protocol::schema::SessionUpdate::UserMessageChunk(
                    agent_client_protocol::schema::ContentChunk::new(
                        agent_client_protocol::schema::ContentBlock::Text(
                            agent_client_protocol::schema::TextContent::new("hello replay"),
                        ),
                    ),
                ),
            )),
        });
        writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        let flush_ev = wrap_event(SpurEventBody::TurnComplete {
            session: spur_acp::SessionId("test-sess".into()),
        });
        writeln!(f, "{}", serde_json::to_string(&flush_ev).unwrap()).unwrap();
        drop(f);

        // Build an empty App via the existing test helper and run replay
        // against it directly, mirroring run_tui_with_license's wiring.
        let mut app = test_app();
        let cfg = spur_core::event_replay::ReplayConfig {
            replay_horizon: std::time::Duration::from_secs(86400 * 365),
            skip_pid: None, // include all PIDs in this test
            ..Default::default()
        };
        let stats = spur_core::event_replay::replay_events(&cfg, |ev| {
            app.lineage.apply(ev);
            app.plan_projection.apply(ev);
            app.synopsis.apply(ev);
        })
        .unwrap();

        assert_eq!(stats.events_applied, 2, "stats: {:?}", stats);
        let synopsis = app
            .synopsis
            .get(&spur_acp::SessionId("test-sess".into()))
            .expect("replay should populate synopsis for test-sess");
        assert_eq!(synopsis.last_user_msg.as_deref(), Some("hello replay"));

        std::env::set_current_dir(cwd).unwrap();
    }

    #[test]
    fn orphan_worker_notification_is_dropped() {
        let mut app = test_app();
        let notif = Box::new(SessionNotification::new("abc", msg_update("orphan")));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "orphan-exec".into(),
            notification: notif,
        }));
        assert!(
            app.worker_streams().get("orphan-exec").is_none(),
            "orphan events must not materialize a trace"
        );
    }

    #[test]
    fn seed_from_stream_buffer_on_rehydrate() {
        use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};
        use std::time::SystemTime;

        let mut ws = crate::worker_streams::WorkerStreams::new();
        let entries = [
            WorkerStreamEntry {
                kind: WorkerStreamKind::Message,
                text: "restored".into(),
                occurred_at: SystemTime::now(),
            },
            WorkerStreamEntry {
                kind: WorkerStreamKind::Thought,
                text: "restored-2".into(),
                occurred_at: SystemTime::now(),
            },
        ];
        ws.seed_from_stream_buffer("restored-exec", "claude", entries.iter());
        let trace = ws.get("restored-exec").expect("seeded trace");
        assert_eq!(trace.entry_count(), 2);
    }

    #[test]
    fn executor_retry_started_resets_trace() {
        let mut app = test_app();
        app.lineage
            .apply(&wrap_event(SpurEventBody::ExecutorSpawned {
                id: "exec-r".into(),
                parent_id: None,
                session_id: SessionId("abc".into()),
                agent: "claude".into(),
                role: spur_acp::Role::Executor,
                task_spec: String::new(),
            }));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "exec-r".into(),
            notification: Box::new(SessionNotification::new("abc", msg_update("pre-retry"))),
        }));
        assert_eq!(app.worker_streams().get("exec-r").unwrap().entry_count(), 1);
        app.handle_spur_event(wrap_event(SpurEventBody::ExecutorRetryStarted {
            id: "exec-r".into(),
            attempt_n: 2,
            reason: "test retry".into(),
            new_session_id: SessionId("new-sess".into()),
        }));
        assert_eq!(
            app.worker_streams().get("exec-r").unwrap().entry_count(),
            0,
            "retry clears the per-executor trace"
        );
    }

    #[test]
    fn app_tick_drives_worker_streams_tick_all() {
        let mut app = test_app();
        app.lineage
            .apply(&wrap_event(SpurEventBody::ExecutorSpawned {
                id: "exec-tick".into(),
                session_id: spur_acp::SessionId("s".into()),
                parent_id: None,
                agent: "claude".into(),
                role: spur_acp::Role::Executor,
                task_spec: String::new(),
            }));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: spur_acp::SessionId("brain-1".into()),
            executor_id: "exec-tick".into(),
            notification: Box::new(spur_acp::SessionNotification::new("s", msg_update("x"))),
        }));

        // Ticking must not panic and must leave the trace queryable.
        app.tick();
        app.tick();
        assert!(app.worker_streams().get("exec-tick").is_some());
    }
}

#[cfg(test)]
mod plan_projection_tests {
    use super::*;
    use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask};

    fn wrap(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    fn spawn_brain(app: &mut App, session: &SessionId) {
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: session.clone(),
        }));
    }

    fn sample_plan_snapshot_event(session: &SessionId) -> SpurEvent {
        wrap(SpurEventBody::PlanSnapshotUpdated {
            session_id: session.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "p-1".into(),
                epic_id: None,
                status: "running".into(),
                progress: "0/1 done".into(),
                next_action:
                    "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                        .into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    pending: 1,
                    ..Default::default()
                },
                tasks: vec![PlanSnapshotTask {
                    task_id: "task-1".into(),
                    task_name: "task-1".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-1".into()),
                    status: "pending".into(),
                    attempt: 0,
                    max_attempts: 3,
                    depends_on: Vec::new(),
                    blocked_by: Vec::new(),
                    unblocks: Vec::new(),
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: None,
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: Vec::new(),
                    next_action: "wait".into(),
                }],
                owner_brain_session_id: None,
                owner_token: None,
                owner_acquired_at: None,
            }),
        })
    }

    #[test]
    fn navigate_to_plan_inspector_and_back_returns_to_session_detail() {
        let mut app = App::new_for_tests();
        let session = SessionId("brain-1".into());
        spawn_brain(&mut app, &session);

        app.process_action(Action::NavigateTo(ViewId::PlanInspector(session.clone())));
        assert!(matches!(app.current_view(), ViewId::PlanInspector(_)));

        app.process_action(Action::NavigateBack);
        assert!(matches!(app.current_view(), ViewId::SessionDetail(_)));
    }

    #[test]
    fn plan_snapshot_event_updates_plan_store() {
        let mut app = App::new_for_tests();
        let session = SessionId("brain-1".into());

        app.handle_spur_event(sample_plan_snapshot_event(&session));

        let plan = app
            .plan_projection()
            .current_for_session(&session)
            .expect("tracked plan");
        assert_eq!(plan.plan_id, "p-1");
        assert_eq!(
            plan.task("task-1").unwrap().issue_id.as_deref(),
            Some("bd-1")
        );
    }
}

#[cfg(test)]
mod brain_retired_tests {
    //! Second-order consumers of `SpurEventBody::BrainRetired` on the App
    //! side. Commit 1 wired the lineage projection; these tests cover the
    //! App-level state that must also react, namely:
    //!
    //! - `brain_name` must null out on retire so readbacks between `/clear`
    //!   and the next prompt are not stale (R5).
    //! - `metadata_store.last_active_*` must be cleared so `/clear` followed
    //!   by a process quit does NOT auto-resume the retired session on the
    //!   next `spur watch` launch (R7; the real user-visible bug).
    //!
    //! These tests exercise private fields, so they live in-module.
    use super::*;
    use spur_acp::domain::events::{BrainRetireReason, SpurEvent, SpurEventBody};
    use spur_acp::SessionId;

    fn wrap(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    /// Construct an `App` with a live `user_input_tx` so tests that go
    /// through `Action::ClearSession` (which requires `tx.try_send` to
    /// succeed for the send-first reset gate) can observe the reset.
    /// Returns the receiver so the channel stays open for the test's
    /// lifetime.
    fn app_with_user_input_tx() -> (App, tokio::sync::mpsc::Receiver<UserInput>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<UserInput>(8);
        (App::new(Some(tx), false), rx)
    }

    fn effort_config_option() -> spur_acp::SessionConfigOption {
        use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

        SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort".to_string()),
            "effort".to_string(),
            "medium".to_string(),
            vec![SessionConfigSelectOption::new(
                "medium".to_string(),
                "Medium".to_string(),
            )],
        )
    }

    fn caps_without_config_options() -> std::sync::Arc<spur_acp::SpurAgentCaps> {
        let init = agent_client_protocol::schema::InitializeResponse::new(
            agent_client_protocol::schema::ProtocolVersion::LATEST,
        );
        let new = agent_client_protocol::schema::NewSessionResponse::new(
            agent_client_protocol::schema::SessionId::new("acp-b1"),
        );
        std::sync::Arc::new(spur_acp::SpurAgentCaps::new(
            &init,
            &new,
            spur_acp::AgentKind::CodexAcp,
        ))
    }

    #[test]
    fn agent_session_ready_installs_caps_on_session_detail() {
        let mut app = App::new_for_tests();
        let session = SessionId("b1".into());
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "codex".into(),
            session: session.clone(),
        }));
        app.handle_spur_event(wrap(SpurEventBody::CommandRegistryDirty {
            session: session.clone(),
            config_options: vec![effort_config_option()],
        }));

        let names_before: Vec<String> = app
            .session_detail
            .as_ref()
            .expect("BrainSpawned must create session detail")
            .available_slash_commands()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert!(
            names_before.iter().any(|name| name == "effort"),
            "precondition: caps=None keeps /effort visible; got {names_before:?}"
        );

        app.handle_spur_event(wrap(SpurEventBody::AgentSessionReady {
            session: session.clone(),
            acp_session_id: "acp-b1".into(),
            brain: "codex".into(),
            resumed: false,
            cancel_mode: spur_acp::CancelMode::AcpSoft,
            fs_unsafe: false,
            caps: Some(caps_without_config_options()),
        }));

        let names_after: Vec<String> = app
            .session_detail
            .as_ref()
            .expect("session detail must remain focused")
            .available_slash_commands()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert!(
            !names_after.iter().any(|name| name == "effort"),
            "AgentSessionReady caps must constrain advertised commands; got {names_after:?}"
        );
    }

    #[test]
    fn brain_retired_nulls_brain_name() {
        let mut app = App::new_for_tests();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));
        assert_eq!(app.brain_name.as_deref(), Some("kiro"));

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::UserClear,
        }));

        assert!(
            app.brain_name.is_none(),
            "brain_name must null on retire so readbacks aren't stale"
        );
    }

    #[test]
    fn brain_retired_clears_last_active_auto_resume_pointers() {
        // Simulates: BrainSpawned → AgentSessionReady writes last_active_*
        // → /clear emits BrainRetired → arm clears last_active_*.
        // Result: spur-cli's `last_active_acp()` returns None on relaunch.
        let mut app = App::new_for_tests();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));
        app.handle_spur_event(wrap(SpurEventBody::AgentSessionReady {
            session: SessionId("b1".into()),
            acp_session_id: "acp-b1".into(),
            brain: "kiro".into(),
            resumed: false,
            cancel_mode: spur_acp::CancelMode::AcpSoft,
            fs_unsafe: false,
            caps: None,
        }));
        assert!(
            app.metadata_store.last_active_acp().is_some(),
            "precondition: AgentSessionReady seeds last_active_acp"
        );

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::UserClear,
        }));

        assert!(
            app.metadata_store.last_active_acp().is_none(),
            "last_active_acp must be cleared on retire so /clear+quit doesn't auto-resume"
        );
    }

    #[test]
    fn clear_session_resets_session_detail_on_successful_send() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));

        let sid_before = app.session_detail.as_ref().unwrap().session_id().clone();
        app.process_action(Action::ClearSession);

        let detail = app.session_detail.as_ref().expect("view must still exist");
        assert!(detail.is_cleared());
        assert!(detail.ready_banner_text().is_some());
        assert_eq!(detail.session_id(), &sid_before, "session_id stays retired");
        assert_eq!(app.brain_status, BrainStatus::Idle);
    }

    #[test]
    fn clear_session_preserves_input_bar_contents() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));
        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("typed before clear".into(), 18);

        app.process_action(Action::ClearSession);

        assert_eq!(
            app.session_detail.as_ref().unwrap().input_bar_text(),
            "typed before clear"
        );
    }

    #[test]
    fn clear_while_streaming_does_not_panic_and_resets_flags() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));
        app.session_detail.as_mut().unwrap().stream_in_flight = true;

        app.process_action(Action::ClearSession);

        let detail = app.session_detail.as_ref().unwrap();
        assert!(!detail.stream_in_flight);
        assert!(detail.is_cleared());
    }

    #[test]
    fn connected_dashboard_first_submit_still_spawns_new_session() {
        let (mut app, mut rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainConnected {
            brain: "kiro".into(),
        }));

        assert_eq!(app.brain_status, BrainStatus::Connected);

        app.handle_crossterm_event_for_test(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('h'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.handle_crossterm_event_for_test(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.handle_crossterm_event_for_test(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        match rx.try_recv() {
            Ok(UserInput::NewSessionWithMessage { blocks, interrupt }) => {
                assert_eq!(blocks.len(), 1, "expected single text block");
                assert!(!interrupt, "plain Enter must not set interrupt");
            }
            Ok(_) => panic!("expected NewSessionWithMessage"),
            Err(err) => panic!("expected queued user input, got {err:?}"),
        }
    }

    #[test]
    fn brain_retired_user_clear_resets_view_defensively() {
        let mut app = App::new_for_tests();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::UserClear,
        }));

        let detail = app.session_detail.as_ref().unwrap();
        assert!(detail.is_cleared());
        assert!(detail.ready_banner_text().is_some());
    }

    #[test]
    fn brain_retired_resume_switch_does_not_reset_view() {
        let mut app = App::new_for_tests();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::ResumeSwitch,
        }));

        let detail = app.session_detail.as_ref().unwrap();
        assert!(
            !detail.is_cleared(),
            "ResumeSwitch must NOT trigger view reset"
        );
        assert!(detail.ready_banner_text().is_none());
    }

    #[test]
    fn draft_carryover_across_clear_to_new_brain_spawn() {
        // Use unique session IDs to avoid cross-test pollution from the
        // shared on-disk metadata store.
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("carryover-a".into()),
        }));
        // Seed session A's saved draft.
        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("draft-A".into(), 7);
        app.process_action(Action::SaveDraft {
            session_id: "carryover-a".into(),
            draft: "draft-A".into(),
        });

        // User submits /clear.
        app.process_action(Action::ClearSession);

        // User types a new prompt into the preserved InputBar.
        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("post-clear-prompt".into(), 17);

        // New brain B spawns.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("carryover-b".into()),
        }));

        // A's saved draft was NOT corrupted.
        let metadata_a_draft = app
            .metadata_store
            .entry("carryover-a")
            .map(|e| e.draft.clone())
            .unwrap_or_default();
        assert_eq!(metadata_a_draft, "draft-A");

        // New view for B has the carryover.
        let detail = app.session_detail.as_ref().unwrap();
        assert_eq!(detail.session_id().0, "carryover-b");
        assert_eq!(detail.input_bar_text(), "post-clear-prompt");
    }

    #[test]
    fn draft_carryover_empty_is_noop() {
        // Use unique session IDs to avoid cross-test pollution from the
        // shared on-disk metadata store.
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("empty-carryover-a".into()),
        }));
        app.process_action(Action::ClearSession);
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("empty-carryover-b".into()),
        }));

        let detail = app.session_detail.as_ref().unwrap();
        assert_eq!(detail.input_bar_text(), "");
        let md = &app.metadata_store;
        assert!(md
            .entry("empty-carryover-a")
            .map(|e| e.draft.clone())
            .unwrap_or_default()
            .is_empty());
        assert!(md
            .entry("empty-carryover-b")
            .map(|e| e.draft.clone())
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn clear_session_banner_cleared_on_next_brain_spawn() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("banner-a".into()),
        }));
        app.process_action(Action::ClearSession);
        assert!(app
            .session_detail
            .as_ref()
            .unwrap()
            .ready_banner_text()
            .is_some());

        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("banner-b".into()),
        }));

        let detail = app.session_detail.as_ref().unwrap();
        assert!(detail.ready_banner_text().is_none());
        assert!(!detail.is_cleared());
    }

    #[test]
    fn clear_end_to_end_flow() {
        let (mut app, _rx) = app_with_user_input_tx();

        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("e2e-a".into()),
        }));
        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("mid-thought".into(), 11);

        app.process_action(Action::ClearSession);
        {
            let d = app.session_detail.as_ref().unwrap();
            assert!(d.is_cleared());
            assert!(d.ready_banner_text().is_some());
            assert_eq!(d.input_bar_text(), "mid-thought");
        }

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("e2e-a".into()),
            reason: BrainRetireReason::UserClear,
        }));
        {
            let d = app.session_detail.as_ref().unwrap();
            assert!(d.is_cleared());
            assert_eq!(d.input_bar_text(), "mid-thought");
        }

        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("explain quicksort".into(), 17);
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("e2e-b".into()),
        }));

        let d = app.session_detail.as_ref().unwrap();
        assert_eq!(d.session_id().0, "e2e-b");
        assert!(!d.is_cleared());
        assert!(d.ready_banner_text().is_none());
        assert_eq!(d.input_bar_text(), "explain quicksort");
    }

    #[test]
    fn double_clear_session_is_idempotent() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("double-a".into()),
        }));
        app.process_action(Action::ClearSession);
        app.process_action(Action::ClearSession);
        let d = app.session_detail.as_ref().unwrap();
        assert!(d.is_cleared());
        assert!(d.ready_banner_text().is_some());
    }

    #[test]
    fn clear_over_resume_banner_takes_precedence() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("resume-banner-a".into()),
        }));
        app.session_detail
            .as_mut()
            .unwrap()
            .show_resume_banner("t".into(), "1s ago".into());

        app.process_action(Action::ClearSession);

        let d = app.session_detail.as_ref().unwrap();
        // reset_for_clear wipes resume_banner; ready_banner is now the only one.
        assert!(
            !d.has_resume_banner(),
            "resume_banner must be cleared by reset_for_clear"
        );
        assert!(d.ready_banner_text().is_some());
    }

    #[test]
    fn clear_mid_tool_call_clears_tool_depth() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("mid-tool-a".into()),
        }));
        {
            let detail = app.session_detail.as_mut().unwrap();
            detail.tool_depth_for_test_mut().insert("t1".into(), 1);
            detail.tool_depth_for_test_mut().insert("t2".into(), 2);
        }

        app.process_action(Action::ClearSession);

        assert!(app
            .session_detail
            .as_ref()
            .unwrap()
            .tool_depth_for_test()
            .is_empty());
    }

    #[test]
    fn debounce_tick_after_clear_does_not_save_to_retired_session() {
        let (mut app, _rx) = app_with_user_input_tx();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("debounce-a".into()),
        }));
        // User had a draft 'draft-A' saved.
        app.process_action(Action::SaveDraft {
            session_id: "debounce-a".into(),
            draft: "draft-A".into(),
        });
        // /clear + new typing.
        app.process_action(Action::ClearSession);
        app.session_detail
            .as_mut()
            .unwrap()
            .input_bar_mut_for_test()
            .set_text("post-clear".into(), 10);
        // Force the debounce to trigger (600ms ago).
        app.session_detail
            .as_mut()
            .unwrap()
            .test_set_last_draft_change(
                std::time::Instant::now() - std::time::Duration::from_millis(600),
            );
        let action = app.session_detail.as_mut().unwrap().draft_save_action();
        assert!(
            action.is_none(),
            "cleared view must not emit SaveDraft from tick"
        );

        // A's draft must still be 'draft-A'.
        assert_eq!(
            app.metadata_store.entry("debounce-a").unwrap().draft,
            "draft-A"
        );
    }

    #[test]
    fn brain_retired_shutdown_does_not_panic() {
        let mut app = App::new_for_tests();
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b1".into()),
        }));

        app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
            session: SessionId("b1".into()),
            reason: BrainRetireReason::Shutdown,
        }));

        let detail = app.session_detail.as_ref().unwrap();
        assert!(!detail.is_cleared());
    }

    #[test]
    fn clear_session_with_no_tx_does_not_reset_view() {
        // Spec §3.6: Action::ClearSession must NOT reset the view when
        // `user_input_tx` is None. No brain retirement can be requested,
        // so a visual reset here would produce a ghost-cleared state
        // (view says "cleared" while the stale brain is still active).
        let mut app = App::new_for_tests(); // user_input_tx = None
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("no-tx-a".into()),
        }));
        // Set brain_status to a distinctive non-Idle value so we can
        // assert it is NOT forced to Idle by the ghost-clear path.
        app.brain_status = BrainStatus::Thinking;

        app.process_action(Action::ClearSession);

        let detail = app.session_detail.as_ref().expect("view must still exist");
        assert!(
            !detail.is_cleared(),
            "view must NOT enter cleared state without a successful send"
        );
        assert!(
            detail.ready_banner_text().is_none(),
            "no ready banner without a successful clear"
        );
        assert_eq!(
            app.brain_status,
            BrainStatus::Thinking,
            "brain_status must be unchanged when send is skipped (not forced to Idle)"
        );
    }

    #[test]
    fn clear_session_with_full_tx_does_not_reset_view() {
        // Spec §3.6: Action::ClearSession must NOT reset the view when
        // `tx.try_send` returns an Err. Dropping the receiver forces
        // `TrySendError::Closed`, which exercises the same Err branch
        // as a saturated channel (both are the send-failure gate).
        let (tx, rx) = tokio::sync::mpsc::channel::<UserInput>(1);
        drop(rx); // subsequent try_send returns TrySendError::Closed
        let mut app = App::new(Some(tx), false);
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("full-tx-a".into()),
        }));
        app.brain_status = BrainStatus::Thinking;

        app.process_action(Action::ClearSession);

        let detail = app.session_detail.as_ref().expect("view must still exist");
        assert!(
            !detail.is_cleared(),
            "view must NOT enter cleared state when send fails"
        );
        assert!(
            detail.ready_banner_text().is_none(),
            "no ready banner without a successful clear"
        );
        assert_eq!(
            app.brain_status,
            BrainStatus::Thinking,
            "brain_status must be unchanged on send failure (not forced to Idle)"
        );
    }
}

#[cfg(test)]
mod feature_gate_tests {
    use super::*;
    use spur_license::{FeatureGateError, FeatureKey, Plan, Tier};

    #[test]
    fn send_message_denied_by_feature_gate_opens_upgrade_modal() {
        let mut app = App::new_for_tests();
        app.feature_gate
            .update_state(&spur_license::LicenseState::inactive("stripped for test"));

        app.process_action(Action::SendMessage {
            session: spur_acp::SessionId("session-1".to_string()),
            blocks: Vec::new(),
            interrupt: false,
        });

        let modal = app
            .upgrade_modal
            .as_ref()
            .expect("denied send-message action must open upgrade modal");
        assert_eq!(modal.required_tier, Some(Plan::Community));
        match &modal.err {
            FeatureGateError::Denied { key, tier } => {
                assert_eq!(*key, FeatureKey::CLI_CORE_EXEC);
                assert_eq!(*tier, Tier::Community);
            }
            other => panic!("unexpected feature gate error: {other:?}"),
        }
    }

    #[test]
    fn show_session_cost_denied_for_community_opens_pro_upgrade_modal() {
        let mut app = App::new_for_tests();

        app.process_action(Action::ShowSessionCost);

        let modal = app
            .upgrade_modal
            .as_ref()
            .expect("community session-cost action must open upgrade modal");
        assert_eq!(modal.required_tier, Some(Plan::Pro));
        match &modal.err {
            FeatureGateError::Denied { key, tier } => {
                assert_eq!(*key, FeatureKey::COST_PRO_PER_PROJECT_TRACKING);
                assert_eq!(*tier, Tier::Community);
            }
            other => panic!("unexpected feature gate error: {other:?}"),
        }
    }
}

#[cfg(test)]
mod quit_shortcut_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    #[test]
    fn first_ctrl_c_opens_quit_confirm_without_exiting() {
        let mut app = App::new_for_tests();

        app.handle_crossterm_event_for_test(ctrl_c());

        assert!(
            app.quit_confirm_visible,
            "first Ctrl+C should open the quit prompt"
        );
        assert!(!app.should_quit, "first Ctrl+C must not exit immediately");
    }

    #[test]
    fn second_ctrl_c_force_quits_from_confirm() {
        let mut app = App::new_for_tests();

        app.handle_crossterm_event_for_test(ctrl_c());
        app.handle_crossterm_event_for_test(ctrl_c());

        assert!(
            app.should_quit,
            "second Ctrl+C should bypass confirmation and exit"
        );
        assert!(
            !app.quit_confirm_visible,
            "force quit should dismiss the confirm dialog"
        );
    }

    #[test]
    fn quit_confirm_accepts_y_and_cancels_on_n() {
        let mut app = App::new_for_tests();

        app.handle_crossterm_event_for_test(ctrl_c());
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(
            !app.quit_confirm_visible,
            "n should dismiss the quit prompt"
        );
        assert!(!app.should_quit, "n must keep the app running");

        app.handle_crossterm_event_for_test(ctrl_c());
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit, "y should confirm quit");
    }

    #[test]
    fn dashboard_esc_no_longer_quits_when_nothing_is_active() {
        let mut app = App::new_for_tests();

        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(
            !app.should_quit,
            "Esc should not exit the app from an empty dashboard"
        );
        assert!(
            !app.quit_confirm_visible,
            "Esc should not open quit confirm from an empty dashboard"
        );
    }

    #[test]
    fn paste_is_ignored_while_app_overlays_are_active() {
        let mut app = App::new_for_tests();

        app.help_visible = true;
        app.handle_crossterm_event(Event::Paste("help".into()));
        assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "");
        app.help_visible = false;

        app.quit_confirm_visible = true;
        app.handle_crossterm_event(Event::Paste("quit".into()));
        assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "");
        app.quit_confirm_visible = false;

        app.palette_visible = true;
        app.handle_crossterm_event(Event::Paste("palette".into()));
        assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "");
        app.palette_visible = false;

        app.collision_modal = Some(CollisionModalState {
            acp_id: "acp-1".into(),
            holder: spur_acp::session_lock::HolderInfo::default(),
        });
        app.handle_crossterm_event(Event::Paste("collision".into()));
        assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "");
        app.collision_modal = None;

        app.handle_crossterm_event(Event::Paste("visible".into()));
        assert_eq!(
            app.dashboard_for_test().input_bar_text_for_test(),
            "visible"
        );
    }

    /// Regression: when `quit_confirm_visible` is true, the upgrade modal
    /// must NOT render even if `upgrade_modal` is `Some`. Otherwise input
    /// (handled by quit_confirm) and visuals (upgrade modal on top)
    /// silently disagree — the user sees the wrong dialog for their keys.
    #[test]
    fn upgrade_modal_render_gate_respects_quit_and_collision_precedence() {
        use crate::components::upgrade_modal::UpgradeModalState;
        use spur_license::{FeatureGateError, FeatureKey, Tier};

        let mut app = App::new_for_tests();
        app.upgrade_modal = Some(UpgradeModalState {
            err: FeatureGateError::Denied {
                key: FeatureKey::CLI_CORE_EXEC,
                tier: Tier::Community,
            },
            required_tier: None,
        });

        // Baseline: nothing else up — upgrade modal should render.
        assert!(
            app.should_render_upgrade_modal(),
            "upgrade modal should render when no higher-precedence modal is up"
        );

        // quit_confirm preempts upgrade modal.
        app.quit_confirm_visible = true;
        assert!(
            !app.should_render_upgrade_modal(),
            "upgrade modal must NOT render when quit_confirm_visible is true"
        );
        app.quit_confirm_visible = false;

        // collision preempts upgrade modal.
        app.collision_modal = Some(CollisionModalState {
            acp_id: "acp-1".into(),
            holder: spur_acp::session_lock::HolderInfo::default(),
        });
        assert!(
            !app.should_render_upgrade_modal(),
            "upgrade modal must NOT render when collision_modal is up"
        );

        // Both up: still suppressed.
        app.quit_confirm_visible = true;
        assert!(
            !app.should_render_upgrade_modal(),
            "upgrade modal must NOT render when quit_confirm and collision are both up"
        );
        app.collision_modal = None;
        app.quit_confirm_visible = false;

        // Back to baseline.
        assert!(app.should_render_upgrade_modal());
    }
}

#[cfg(test)]
mod synopsis_wire_tests {
    use super::*;
    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use spur_acp::{SessionId, SessionInfo};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn wrap(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    fn user_message(session: &str, text: &str) -> SpurEvent {
        wrap(SpurEventBody::AgentNotification {
            session: SessionId(session.into()),
            notification: Box::new(SessionNotification::new(
                agent_client_protocol::schema::SessionId::new(session),
                SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(text),
                ))),
            )),
        })
    }

    fn session(id: &str, title: &str) -> SessionInfo {
        SessionInfo::new(id.to_string(), PathBuf::from("/tmp")).title(title.to_string())
    }

    fn app_in_picker_with_empty_metadata() -> App {
        let tmp = NamedTempFile::new().unwrap();
        let mut app = App::new_for_tests();
        app.set_metadata_store_for_test(SessionMetadataStore::load(tmp.path()));
        app.process_action(Action::RequestSessions);
        app
    }

    fn type_picker_search(app: &mut App, query: &str) {
        app.handle_crossterm_event(Event::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        )));
        for ch in query.chars() {
            app.handle_crossterm_event(Event::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )));
        }
    }

    #[test]
    fn handle_spur_event_applies_to_synopsis_projection() {
        let mut app = App::new_for_tests();

        app.handle_spur_event(user_message("S1", "hello world"));

        let s = app
            .synopsis()
            .get(&SessionId("S1".into()))
            .expect("commit-on-read fallback");
        assert_eq!(s.last_user_msg.as_deref(), Some("hello world"));
    }

    #[test]
    fn picker_filter_picks_up_late_synopsis_updates_without_refresh() {
        let mut app = app_in_picker_with_empty_metadata();
        app.handle_spur_event(wrap(SpurEventBody::SessionsListed {
            agent: "claude".into(),
            sessions: vec![session("S1", "Build fix")],
        }));

        app.handle_spur_event(user_message("S1", "late synopsis needle"));
        type_picker_search(&mut app, "needle");

        let picker = app.session_picker_for_test().expect("picker open");
        assert_eq!(
            picker.visible_session_count(app.synopsis()),
            1,
            "filter should see synopsis content applied after SessionsListed"
        );
    }

    #[test]
    fn picker_filter_picks_up_rename_without_refresh() {
        let mut app = app_in_picker_with_empty_metadata();
        app.handle_spur_event(wrap(SpurEventBody::SessionsListed {
            agent: "claude".into(),
            sessions: vec![session("S1", "Old title")],
        }));

        app.process_action(Action::RenameSession {
            session_id: "S1".into(),
            new_title: "renamed recall needle".into(),
            original_title: "Old title".into(),
        });
        type_picker_search(&mut app, "needle");

        let picker = app.session_picker_for_test().expect("picker open");
        assert_eq!(
            picker.visible_session_count(app.synopsis()),
            1,
            "filter should see title_override applied after SessionsListed"
        );
    }
}

#[cfg(test)]
mod theme_threading_tests {
    use super::*;
    use crate::theme::runtime::test_support::with_isolated_dirs;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app_with_theme(theme: &str) -> App {
        let mut spur_config = spur_acp::SpurConfig::default();
        spur_config.tui.theme = theme.to_string();

        App::new_with_config(
            None,
            false,
            std::sync::Arc::new(spur_config),
            crate::landing::LandingDecision::ShowDashboard,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Boots `App` with `tui.theme = "light"` and confirms (a) construction
    /// does not panic even though no surface consumes the theme yet, and
    /// (b) the resolved theme is the requested one. This guards the cascade
    /// from regressing into a `dark`-only fallback path.
    ///
    /// Wrapped in `with_isolated_dirs` so a stray `~/.spur/themes/light.yaml`
    /// or `.spur/themes/light.yaml` in the developer's environment cannot
    /// shadow the built-in and break the assertion.
    #[test]
    fn light_theme_boots_without_panic() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "light".to_string();

            let app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            assert_eq!(app.theme.name, "light");
        });
    }

    #[test]
    fn unknown_theme_falls_back_to_dark_without_panic() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "definitely-not-a-theme".to_string();

            let app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            assert_eq!(app.theme.name, "dark");
        });
    }

    /// `/theme light` must atomically swap `App.theme` so the next render
    /// pulls tokens from the light palette. Verifies both the resolved
    /// `theme.name` and the tracked `active_theme_name` after dispatch.
    #[test]
    fn slash_theme_switch_swaps_app_theme_arc() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "dark".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "light".to_string(),
            });

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
            let hint = app
                .transient_hint_for_test()
                .expect("flash hint set on success");
            assert!(
                hint.text.contains("light"),
                "hint should mention switched theme, got `{}`",
                hint.text
            );
        });
    }

    /// `/theme definitely-not-a-theme` keeps the previous theme intact
    /// and surfaces an error via the transient-hint mechanism. The
    /// `Arc<Theme>` must NOT be replaced — verified via pointer equality.
    #[test]
    fn slash_theme_switch_unknown_keeps_previous_and_flashes_error() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "light".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );
            let prev_theme_ptr = std::sync::Arc::as_ptr(&app.theme);
            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "definitely-not-a-theme".to_string(),
            });

            assert_eq!(app.theme.name, "light", "theme must not change on failure");
            assert_eq!(
                app.active_theme_name, "light",
                "active_theme_name must not change on failure"
            );
            assert_eq!(
                std::sync::Arc::as_ptr(&app.theme),
                prev_theme_ptr,
                "Arc<Theme> must not be replaced on failed switch"
            );
            let hint = app
                .transient_hint_for_test()
                .expect("flash hint set on failure");
            assert!(
                hint.text.contains("not found"),
                "error hint should mention not-found, got `{}`",
                hint.text
            );
        });
    }

    #[test]
    fn bare_slash_theme_opens_theme_picker() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "dark".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });

            assert!(
                app.dashboard_for_test().completion_active(),
                "bare `/theme` should open the fuzzy theme picker"
            );
            assert!(
                app.transient_hint_for_test().is_none(),
                "bare `/theme` should not show the old theme-list flash"
            );
        });
    }

    #[test]
    fn theme_picker_accept_switches_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            app.handle_crossterm_event_for_test(key(KeyCode::Down));
            app.handle_crossterm_event_for_test(key(KeyCode::Enter));

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
        });
    }

    #[test]
    fn theme_picker_esc_cancels_without_changing_dashboard_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            assert!(app.dashboard_for_test().completion_active());

            app.handle_crossterm_event_for_test(key(KeyCode::Esc));

            assert!(!app.dashboard_for_test().completion_active());
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");
        });
    }

    #[test]
    fn slash_theme_reload_reloads_active_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("light");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "reload".to_string(),
            });

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
            let hint = app
                .transient_hint_for_test()
                .expect("reload should flash status");
            assert_eq!(hint.text, "theme reloaded: light");
        });
    }

    #[test]
    fn session_detail_theme_picker_accept_switches_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");
            let session_id = spur_acp::SessionId("palette-test".into());
            app.session_detail = Some(
                crate::views::session_detail::SessionDetailView::new_for_palette_test(
                    crate::commands::CommandRegistry::default(),
                ),
            );
            app.current_view = ViewId::SessionDetail(session_id);

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            assert!(
                app.session_detail
                    .as_ref()
                    .is_some_and(|detail| detail.completion_active()),
                "bare `/theme` should open the session detail theme picker"
            );

            app.handle_crossterm_event_for_test(key(KeyCode::Down));
            app.handle_crossterm_event_for_test(key(KeyCode::Enter));

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
        });
    }

    #[test]
    fn bare_theme_in_unwired_view_flashes_theme_status() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");
            app.current_view = ViewId::SessionPicker;

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });

            let hint = app
                .transient_hint_for_test()
                .expect("unwired views should show theme status");
            assert!(
                hint.text.contains("themes:"),
                "theme status flash should list themes, got `{}`",
                hint.text
            );
            assert!(
                hint.text.contains("* dark"),
                "active theme marker should include a space, got `{}`",
                hint.text
            );
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");
        });
    }
}
