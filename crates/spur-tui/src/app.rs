use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
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
    /// Request full issue detail from the PM backend.
    GetIssueDetail {
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
pub struct LiveCostCache {
    pub by_session: std::collections::HashMap<SessionId, f64>,
    pub last_refresh: chrono::DateTime<chrono::Utc>,
    pub last_error: Option<std::sync::Arc<anyhow::Error>>,
}

#[cfg(feature = "analytics")]
impl Default for LiveCostCache {
    fn default() -> Self {
        Self {
            by_session: std::collections::HashMap::new(),
            last_refresh: chrono::Utc::now(),
            last_error: None,
        }
    }
}

pub struct App {
    current_view: ViewId,
    dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    session_picker: Option<SessionPickerView>,
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
    dirty: bool,
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
    lineage: ExecutorLineage,
    #[cfg(feature = "analytics")]
    analytics_engine: Option<spur_context::AsyncEngine>,
    #[cfg(feature = "analytics")]
    live_cost_cache: Option<std::sync::Arc<RwLock<LiveCostCache>>>,
    #[cfg(feature = "analytics")]
    live_cost_active_sessions: Option<std::sync::Arc<RwLock<std::collections::HashSet<SessionId>>>>,
    #[cfg(feature = "analytics")]
    live_cost_signal_tx: Option<mpsc::Sender<()>>,
    #[cfg(feature = "analytics")]
    live_cost_handle: Option<JoinHandle<()>>,
    /// Lazily constructed on first `Action::OpenInsights`. None until the
    /// user presses Alt+a (or `analytics_engine` is otherwise initialised).
    #[cfg(feature = "analytics")]
    insights_view: Option<crate::views::insights::InsightsView>,
    /// In-flight cold-init for the analytics engine. While `Some`, the
    /// Insights view renders an "indexing logs..." placeholder; the
    /// `oneshot::Receiver` is polled on tick and resolves to either the
    /// constructed `AsyncEngine` (success) or an error string (failure).
    /// Cleared once the outcome is consumed in either branch. Bug A:
    /// without this the init ran synchronously on the UI thread for ~89s
    /// on first open, freezing the entire TUI.
    #[cfg(feature = "analytics")]
    insights_init: Option<InsightsInitState>,
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

#[cfg(feature = "analytics")]
struct InsightsInitState {
    started_at: Instant,
    rx: tokio::sync::oneshot::Receiver<anyhow::Result<(spur_context::AsyncEngine, bool)>>,
    /// Whole-second elapsed value last shown on the placeholder. Used to
    /// throttle redraws to 1Hz when init is in flight (instead of forcing
    /// dirty on every 30Hz tick). Initialized to `u64::MAX` so the first
    /// drain after a `Some` insertion always paints.
    last_displayed_second: u64,
}

/// Render the cold-init placeholder shown when the user has switched to
/// `ViewId::Insights` but the analytics engine is still being built on
/// the background `spawn_blocking` worker. Uses the body area provided
/// by the App's view-render dispatch (the global header/footer rows are
/// already drawn around it).
#[cfg(feature = "analytics")]
fn render_insights_init_placeholder(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    started_at: Instant,
) {
    use ratatui::{
        layout::Alignment,
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };

    let elapsed = started_at.elapsed().as_secs();
    let title = Line::from(Span::styled(
        "Indexing logs from agent JSONL files…",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let progress = Line::from(Span::styled(
        format!("Elapsed: {elapsed}s   (~90s typical on first open; warm runs are sub-second)"),
        Style::default().fg(Color::Gray),
    ));
    let hint = Line::from(Span::styled(
        "[Esc] return to Dashboard  (indexing continues in background)",
        Style::default().fg(Color::DarkGray),
    ));
    let body = vec![
        Line::from(""),
        title,
        Line::from(""),
        progress,
        Line::from(""),
        hint,
    ];
    frame.render_widget(Paragraph::new(body).alignment(Alignment::Center), area);
}

/// Cold init pipeline for the analytics engine. Blocks (DuckDB I/O +
/// JSONL scan); ALWAYS run inside `tokio::task::spawn_blocking`.
#[cfg(feature = "analytics")]
fn build_analytics_engine_blocking() -> anyhow::Result<(spur_context::AsyncEngine, bool)> {
    use spur_context::{AnalyticsEngine, AsyncEngine};

    let t0 = std::time::Instant::now();
    let cache_dir = directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".spur").join("cache"))
        .unwrap_or_else(|| std::path::PathBuf::from(".spur/cache"));
    std::fs::create_dir_all(&cache_dir)?;
    let cache_path = cache_dir.join("cost.duckdb");
    tracing::info!(target: "spur_tui::insights", path = %cache_path.display(), "opening DuckDB cache (background)");

    let (engine, recovered) = AnalyticsEngine::open(&cache_path)?;
    engine.initialize()?;
    let view_status = engine.create_agent_views()?;
    engine.load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())?;
    let materialized = engine.refresh_cache()?;
    engine.use_cached_events()?;
    tracing::info!(
        target: "spur_tui::insights",
        total_ms = t0.elapsed().as_millis() as u64,
        materialized_rows = materialized,
        ?view_status,
        "analytics engine cold init done"
    );
    Ok((AsyncEngine::new(engine), recovered))
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
    ) -> Self {
        Self::build_with_license_state(
            user_input_tx,
            start_in_picker.then_some(None),
            config,
            license_state,
            landing,
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
        )
    }

    fn build_with_license_state(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
    ) -> Self {
        let metadata_path = std::path::PathBuf::from(".spur").join("session_metadata.json");
        Self::build_with_license_state_from_metadata_path(
            user_input_tx,
            start_in_picker_with_preselect,
            config,
            license_state,
            landing,
            metadata_path,
        )
    }

    fn build_with_license_state_from_metadata_path(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        metadata_path: std::path::PathBuf,
    ) -> Self {
        let metadata_store = SessionMetadataStore::load(&metadata_path);
        let start_in_picker = start_in_picker_with_preselect.is_some();

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
            dashboard,
            session_detail: None,
            session_picker,
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

    pub fn flash_hint(&mut self, msg: impl Into<String>, duration: Duration) {
        self.transient_hint = Some(TransientHint {
            text: msg.into(),
            expires_at: Instant::now() + duration,
        });
        self.dirty = true;
    }

    pub fn flash_hint_short(&mut self, msg: impl Into<String>) {
        self.flash_hint(msg, Duration::from_secs(2));
    }

    fn tick_transient_hint(&mut self, now: Instant) {
        if self
            .transient_hint
            .as_ref()
            .is_some_and(|hint| now >= hint.expires_at)
        {
            self.transient_hint = None;
            self.dirty = true;
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn transient_hint_for_test(&self) -> Option<&TransientHint> {
        self.transient_hint.as_ref()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn flash_hint_short_for_test(&mut self, msg: &str) {
        self.flash_hint_short(msg);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn flash_hint_for_test(&mut self, msg: &str, duration: Duration) {
        self.flash_hint(msg, duration);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn tick_transient_hint_for_test(&mut self, now: Instant) {
        self.tick_transient_hint(now);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn transient_hint_text(&self) -> Option<&str> {
        self.transient_hint.as_ref().map(|hint| hint.text.as_str())
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
    pub fn is_help_visible_for_test(&self) -> bool {
        self.help_visible
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_quit_confirm_visible_for_test(&self) -> bool {
        self.quit_confirm_visible
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_collision_modal_visible_for_test(&self) -> bool {
        self.collision_modal.is_some()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_collision_modal_for_test(
        &mut self,
        acp_id: impl Into<String>,
        holder: spur_acp::session_lock::HolderInfo,
    ) {
        self.collision_modal = Some(CollisionModalState {
            acp_id: acp_id.into(),
            holder,
        });
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_upgrade_modal_visible_for_test(&self) -> bool {
        self.upgrade_modal.is_some()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_upgrade_modal_for_test(
        &mut self,
        err: spur_license::FeatureGateError,
        required_tier: Option<spur_license::Plan>,
    ) {
        self.upgrade_modal = Some(UpgradeModalState { err, required_tier });
    }

    #[cfg(any(test, debug_assertions))]
    pub fn current_view_for_test(&self) -> &ViewId {
        &self.current_view
    }

    #[cfg(any(test, debug_assertions))]
    pub fn age_esc_chain_for_test(&mut self, duration: Duration) {
        for instant in &mut self.esc_chain {
            if let Some(aged) = instant.checked_sub(duration) {
                *instant = aged;
            }
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn esc_chain_len_for_test(&self) -> usize {
        self.esc_chain.len()
    }

    fn update_license_state(&mut self, license_state: LicenseStateEvent) {
        let resolved = license_state_event_to_state(&license_state);
        self.feature_gate.update_state(&resolved);
        self.license_badge = license_badge_from_state(&license_state);
        self.license_state = license_state;
        self.dirty = true;
    }

    fn open_palette(&mut self) {
        if self.help_visible
            || self.quit_confirm_visible
            || self.collision_modal.is_some()
            || self.upgrade_modal.is_some()
        {
            return; // palette won't open while a higher-priority overlay is up
        }
        tracing::debug!(target: "palette", "open_palette: start");
        self.palette_state.reset();

        // Load sources: Views, Commands, Sessions, Workers. (Trace deferred — see U3c.)
        // CommandRegistry is not Clone; borrow from the active session_detail
        // or fall back to a fresh empty one (SpurLocal commands are still
        // included unconditionally via registry's ensure_cache).
        //
        // IMPORTANT — DO NOT "SIMPLIFY":
        // `owned_fallback` is declared on its own line BEFORE the match so its
        // storage outlives the `&owned_fallback` reference produced inside the
        // `None` arm. Rewriting this as `match ... { None => &CommandRegistry::new() }`
        // will NOT compile: the temporary returned by `CommandRegistry::new()`
        // would be dropped at the end of the arm, leaving a dangling reference.
        // This idiom intentionally trades two extra lines for a stable borrow.
        let owned_fallback;
        let cmd_registry: &crate::commands::registry::CommandRegistry =
            match self.session_detail.as_ref() {
                Some(view) => &view.command_registry,
                None => {
                    owned_fallback = crate::commands::registry::CommandRegistry::new();
                    &owned_fallback
                }
            };
        let view_src = ViewSource;
        let cmd_src = CommandSource::new(cmd_registry);
        let sess_src = SessionSource::from_metadata(self.metadata_store.metadata());
        let worker_src = WorkerSource::from_lineage(&self.lineage);

        let view_batch = view_src.collect();
        let cmd_batch = cmd_src.collect();
        let sess_batch = sess_src.collect();
        let worker_batch = worker_src.collect();
        // Trace source is unconditionally omitted (U3c) — log the deferral
        // state, not session presence, so telemetry stays honest.
        let trace_dispatch_deferred = true;
        tracing::debug!(
            target: "palette",
            commands = cmd_batch.len(),
            sessions = sess_batch.len(),
            workers = worker_batch.len(),
            trace_dispatch_deferred,
            "open_palette: sources collected"
        );
        let batches = vec![view_batch, cmd_batch, sess_batch, worker_batch];
        // Trace source is intentionally skipped until trace-dispatch lands;
        // see docs/superpowers/specs/2026-04-20-palette-end-to-end-integration-design.md (U3c).
        // TODO(palette-trace-dispatch): re-add a TraceSource batch here when
        // Action::ScrollToTraceEntry lands with a stable-id design for TraceEntry.
        self.palette_state.extend_raw(batches);

        self.palette_visible = true;
        self.dirty = true;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_palette_visible(&self) -> bool {
        self.palette_visible
    }

    #[cfg(any(test, debug_assertions))]
    pub fn try_open_palette_for_test(&mut self) {
        self.open_palette();
    }

    #[cfg(any(test, debug_assertions))]
    pub fn seed_palette_with_session_for_test(&mut self, session_id: &str, label: &str) {
        use crate::components::palette::{PaletteKind, PalettePayload, PaletteResult};
        // Reset first so the injected result is the only one in the list.
        self.palette_state.reset();
        self.palette_state.push_raw(vec![PaletteResult {
            kind: PaletteKind::Session,
            label: label.to_string(),
            subtitle: format!("session · {}", session_id),
            payload: PalettePayload::Session {
                session_id: session_id.to_string(),
            },
        }]);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn last_action_for_test(&self) -> Option<crate::action::Action> {
        self.last_action.clone()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn palette_state_for_test(&self) -> &crate::components::palette::PaletteState {
        &self.palette_state
    }

    #[cfg(any(test, debug_assertions))]
    pub fn palette_state_for_test_mut(&mut self) -> &mut crate::components::palette::PaletteState {
        &mut self.palette_state
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
    pub fn user_warning_for_test(&self) -> Option<&str> {
        self.user_warning.as_deref()
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

    #[cfg(any(test, debug_assertions))]
    pub fn new_for_palette_test() -> Self {
        // Minimal App for palette-integration tests. Uses the same path as
        // test_support::new_app — no user_input channel, no session picker —
        // so open_palette() can run safely.
        Self::new(None, false)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn seed_session_detail_with_dynamic_command_for_test(
        &mut self,
        handle: &str,
        name: &str,
        description: &str,
    ) {
        use crate::commands::registry::CommandRegistry;
        use spur_acp::{AvailableCommand, CommandsConfig};
        let cfg = CommandsConfig::default();
        let entry =
            crate::agents::build_entry(handle, &cfg, &AvailableCommand::new(name, description));
        let mut registry = CommandRegistry::new();
        registry.set_agent_commands(handle, vec![entry]);
        self.session_detail =
            Some(crate::views::session_detail::SessionDetailView::new_for_palette_test(registry));
    }

    /// Current ACP session id, if a `session_detail` is active.
    /// Used by the palette's `Command` accept path to construct
    /// `Action::SendMessage` / `Action::VendorExec` without a round-trip
    /// through the session-detail view.
    fn current_acp_session_id(&self) -> Option<spur_acp::SessionId> {
        self.session_detail.as_ref().map(|v| v.session_id().clone())
    }

    fn result_to_action(
        &self,
        result: crate::components::palette::PaletteResult,
    ) -> Option<crate::action::Action> {
        use crate::action::{Action, ViewId};
        use crate::commands::registry::CommandRegistry;
        use crate::commands::submit_router::{route, SubmitDecision};
        use crate::components::palette::PalettePayload;
        match result.payload {
            PalettePayload::View { action } => Some(action),
            PalettePayload::Session { session_id } => Some(Action::ResumeSession { session_id }),
            PalettePayload::Worker { session_id } => {
                Some(Action::NavigateTo(ViewId::SessionDetail(session_id)))
            }
            PalettePayload::Command { name } => {
                // IMPORTANT — DO NOT "SIMPLIFY":
                // `owned_fallback` is declared on its own line BEFORE the match so its
                // storage outlives the `&owned_fallback` reference returned from the
                // `None` arm. Rewriting this as `match ... { None => &CommandRegistry::new() }`
                // will NOT compile: the temporary returned by `CommandRegistry::new()`
                // would be dropped at the end of the arm, leaving a dangling reference.
                // This idiom is intentionally identical to the one in `open_palette`
                // (CommandRegistry is not Clone, so we can't sidestep with .clone()).
                let owned_fallback;
                let registry: &CommandRegistry = match self.session_detail.as_ref() {
                    Some(view) => &view.command_registry,
                    None => {
                        owned_fallback = CommandRegistry::new();
                        &owned_fallback
                    }
                };
                match route(&format!("/{name}"), &[], registry, false) {
                    SubmitDecision::Local { action } => Some(action),
                    SubmitDecision::Send { blocks, interrupt } => {
                        let session = self.current_acp_session_id()?;
                        Some(Action::SendMessage {
                            session,
                            blocks,
                            interrupt,
                        })
                    }
                    SubmitDecision::VendorExec { method, params } => {
                        let session = self.current_acp_session_id()?;
                        Some(Action::VendorExec {
                            session,
                            method,
                            params,
                        })
                    }
                    SubmitDecision::SetSessionConfigOption { config_id, value } => {
                        Some(Action::SetSessionConfigOption { config_id, value })
                    }
                    SubmitDecision::SetSessionModel { value } => {
                        let session_id = self.current_acp_session_id()?;
                        Some(Action::SetSessionModel { session_id, value })
                    }
                    SubmitDecision::Empty => None,
                }
            }
            PalettePayload::Trace { entry_idx: _ } => {
                // TODO(palette-trace-dispatch): wire when stable-id design lands.
                // Unreachable in practice because TraceSource is omitted from
                // extend_raw (see open_palette). Kept as a type-exhaustiveness
                // anchor and a forward-compat hook.
                None
            }
        }
    }

    /// Look up the `AgentConfig` for an agent by name (`AgentConfig::name`)
    /// in the loaded `SpurConfig`. Falls back to a minimal synthesized
    /// config when the agent isn't declared — this preserves startup
    /// behavior when no `.spur/config.toml` is present.
    fn resolve_agent_config(&self, name: &str) -> std::sync::Arc<spur_acp::AgentConfig> {
        self.config
            .agents
            .entries
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .map(std::sync::Arc::new)
            .unwrap_or_else(|| {
                tracing::warn!(
                    agent = %name,
                    "agent not found in config.toml — using PromptText fallback; \
                     vendor-ext commands will not be registered"
                );
                std::sync::Arc::new(Self::fallback_agent_config(name))
            })
    }

    fn fallback_agent_config(name: &str) -> spur_acp::AgentConfig {
        spur_acp::AgentConfig::with_defaults(name)
    }

    /// Derive the `WorkerMentionDescriptor` snapshot from the loaded
    /// agent config. Filtered to roles that can serve as a worker
    /// (matches `AgentRegistry::worker_capable` semantics).
    fn build_worker_snapshot(&self) -> Vec<crate::mentions::WorkerMentionDescriptor> {
        use spur_acp::config::Tier;
        use spur_acp::types::AgentRole;
        self.config
            .agents
            .entries
            .iter()
            .filter(|cfg| matches!(cfg.role, AgentRole::Worker | AgentRole::Both))
            .map(|cfg| crate::mentions::WorkerMentionDescriptor {
                name: cfg.name.clone(),
                description: cfg.delegation.description.clone(),
                tier: cfg.delegation.tier.map(|t| match t {
                    Tier::Specialist => "specialist".to_string(),
                    Tier::Generalist => "generalist".to_string(),
                }),
            })
            .collect()
    }

    /// Refresh Dashboard's worker mention snapshot from the current app config.
    /// This is the canonical hook point for any future config-reload event.
    pub(crate) fn sync_dashboard_workers(&mut self) {
        let workers = self.build_worker_snapshot();
        self.dashboard.set_worker_snapshot(workers);
    }

    /// Lazily open the shared DuckDB analytics cache, materialise per-agent
    /// Kick off (or no-op) the analytics-engine cold init.
    ///
    /// Returns immediately. If no init is needed (engine already cached
    /// or insights_view already constructed), nothing happens. Otherwise
    /// spawns a `spawn_blocking` task that runs the heavy DuckDB pipeline
    /// (open / initialize / create_agent_views / load_pricing /
    /// refresh_cache / use_cached_events) on a worker thread and posts
    /// the resulting `AsyncEngine` (or error) through a `oneshot` that
    /// the App's tick path drains. While in flight, the Insights view
    /// renders an "indexing logs..." placeholder.
    ///
    /// Cold first run can take ~90s (full JSONL scan across all agent
    /// homes); warm runs reuse the cache at `~/.spur/cache/cost.duckdb`
    /// and return in milliseconds. Shares the cache path with `spur cost`
    /// so a prior CLI invocation primes the data.
    #[cfg(feature = "analytics")]
    fn start_insights_init(&mut self) {
        if self.insights_view.is_some() || self.insights_init.is_some() {
            return;
        }

        if let Some(existing) = self.analytics_engine.clone() {
            // Engine already built (e.g., earlier cold init for the
            // dashboard's live-cost cache). Construct the view directly.
            self.insights_view = Some(crate::views::insights::InsightsView::new(existing));
            return;
        }

        tracing::info!(target: "spur_tui::insights", "start_insights_init: dispatching cold init to spawn_blocking");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(build_analytics_engine_blocking());
        });
        self.insights_init = Some(InsightsInitState {
            started_at: Instant::now(),
            rx,
            last_displayed_second: u64::MAX,
        });
    }

    /// Drain a completed `insights_init` outcome, if any. Called from the
    /// tick path. On success: caches the engine, constructs the view,
    /// keeps `current_view = Insights` so the user sees the populated
    /// dashboard. On failure: surfaces a warning and routes back to
    /// Dashboard.
    #[cfg(feature = "analytics")]
    fn drain_insights_init(&mut self) {
        let Some(mut state) = self.insights_init.take() else {
            return;
        };
        match state.rx.try_recv() {
            Ok(Ok((engine, recovered))) => {
                tracing::info!(target: "spur_tui::insights", elapsed_ms = state.started_at.elapsed().as_millis() as u64, "insights init complete; constructing view");
                self.analytics_engine = Some(engine.clone());
                self.insights_view = Some(crate::views::insights::InsightsView::new(engine));
                if recovered {
                    self.show_user_warning(
                        "Analytics WAL was corrupt; renamed to *.broken and re-opened. Last refresh window may be missing."
                            .to_string(),
                    );
                }
                self.dirty = true;
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "spur_tui::insights", error = %format!("{e:#}"), "insights init failed");
                self.show_user_warning(format!("Analytics unavailable: {e:#}"));
                if matches!(self.current_view, ViewId::Insights) {
                    self.current_view = ViewId::Dashboard;
                }
                self.dirty = true;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                // Still in flight. Throttle redraws to 1Hz: only mark
                // dirty when the user is actually viewing the placeholder
                // AND the displayed whole-second has advanced. Avoids
                // 30Hz × 90s = ~2700 wasted redraws when the user has
                // Esc'd back to Dashboard while init continues.
                let elapsed = state.started_at.elapsed().as_secs();
                let visible = matches!(self.current_view, ViewId::Insights);
                if visible && elapsed != state.last_displayed_second {
                    state.last_displayed_second = elapsed;
                    self.dirty = true;
                }
                self.insights_init = Some(state);
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                tracing::warn!(target: "spur_tui::insights", "insights init worker channel closed without sending result");
                self.show_user_warning(
                    "Analytics init worker exited before reporting a result".to_string(),
                );
                if matches!(self.current_view, ViewId::Insights) {
                    self.current_view = ViewId::Dashboard;
                }
                self.dirty = true;
            }
        }
    }

    #[cfg(feature = "analytics")]
    fn sync_live_cost_active_sessions(&mut self) {
        let active_sessions: std::collections::HashSet<SessionId> = self
            .lineage
            .nodes()
            .filter(|node| {
                matches!(
                    node.phase,
                    spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning
                )
            })
            .filter_map(|node| {
                node.current_attempt()
                    .map(|attempt| attempt.session_id.clone())
            })
            .collect();

        let changed = self
            .live_cost_active_sessions
            .as_ref()
            .and_then(|shared| shared.try_write().ok())
            .map(|mut guard| {
                if *guard == active_sessions {
                    false
                } else {
                    *guard = active_sessions;
                    true
                }
            })
            .unwrap_or(false);

        if changed {
            if let Some(tx) = &self.live_cost_signal_tx {
                let _ = tx.try_send(());
            }
        }
    }

    #[cfg(feature = "analytics")]
    fn spawn_live_cost_refresh(&mut self) {
        if self.live_cost_handle.is_some() {
            return;
        }

        let Some(engine) = self.analytics_engine.clone() else {
            return;
        };
        let Some(cache) = self.live_cost_cache.clone() else {
            return;
        };
        let Some(active_sessions) = self.live_cost_active_sessions.clone() else {
            return;
        };

        let (signal_tx, mut signal_rx) = mpsc::channel(8);
        self.live_cost_signal_tx = Some(signal_tx);
        self.live_cost_handle = Some(tokio::spawn(async move {
            loop {
                let interval = {
                    let guard = active_sessions.read().await;
                    if guard.is_empty() {
                        Duration::from_secs(30)
                    } else {
                        Duration::from_secs(5)
                    }
                };

                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    opt = signal_rx.recv() => {
                        if opt.is_none() {
                            return;
                        }
                    }
                }

                let active_ids: Vec<SessionId> =
                    active_sessions.read().await.iter().cloned().collect();
                let refresh = engine.run(move |e| {
                    let mut out = std::collections::HashMap::new();
                    for sid in active_ids {
                        if let Some(snapshot) = e.live_session_snapshot(&sid.0)? {
                            out.insert(sid, snapshot.cost_usd);
                        }
                    }
                    Ok(out)
                });

                // Timeout stops waiting; it does not cancel AsyncEngine's
                // spawn_blocking closure, which will still run to completion.
                let result = tokio::time::timeout(Duration::from_secs(30), refresh).await;
                let mut guard = cache.write().await;
                match result {
                    Ok(Ok(costs)) => {
                        guard.by_session = costs;
                        guard.last_refresh = chrono::Utc::now();
                        guard.last_error = None;
                    }
                    Ok(Err(error)) => {
                        guard.last_error = Some(std::sync::Arc::new(error));
                    }
                    Err(_) => {
                        guard.last_error = Some(std::sync::Arc::new(anyhow::anyhow!(
                            "live cost refresh timed out (30s)"
                        )));
                    }
                }
            }
        }));
    }

    #[cfg(feature = "analytics")]
    pub async fn shutdown_analytics(&mut self) {
        self.insights_view.take();
        self.live_cost_signal_tx.take();
        if let Some(handle) = self.live_cost_handle.take() {
            handle.abort();
        }

        let Some(engine) = self.analytics_engine.clone() else {
            return;
        };
        match timeout(Duration::from_secs(2), engine.run(|e| e.checkpoint())).await {
            Ok(Ok(())) => {
                tracing::debug!(target: "spur_tui::insights", "analytics checkpoint completed during shutdown");
            }
            Ok(Err(error)) => {
                tracing::warn!(target: "spur_tui::insights", error = %format!("{error:#}"), "analytics checkpoint failed during shutdown");
            }
            Err(_) => {
                tracing::warn!(target: "spur_tui::insights", "analytics checkpoint timed out during shutdown");
            }
        }
    }

    #[cfg(not(feature = "analytics"))]
    pub async fn shutdown_analytics(&mut self) {}

    #[cfg(feature = "analytics")]
    fn via_analytics_visible_for_current_view(&self) -> bool {
        let Some(cache) = &self.live_cost_cache else {
            return false;
        };
        let Ok(guard) = cache.try_read() else {
            return false;
        };

        match &self.current_view {
            ViewId::Dashboard => {
                if let Some(node_id) = self.dashboard.focused_node() {
                    return self
                        .lineage
                        .node(node_id)
                        .and_then(|node| node.current_attempt())
                        .is_some_and(|attempt| guard.by_session.contains_key(&attempt.session_id));
                }
                self.lineage
                    .nodes()
                    .filter_map(|node| node.current_attempt())
                    .any(|attempt| guard.by_session.contains_key(&attempt.session_id))
            }
            ViewId::SessionDetail(session) | ViewId::PlanInspector(session) => {
                guard.by_session.contains_key(session)
            }
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(session) => guard.by_session.contains_key(session),
            ViewId::SessionPicker | ViewId::IssueBrowser | ViewId::Insights => false,
        }
    }

    fn show_user_warning(&mut self, message: String) {
        self.user_warning = Some(message);
        self.dirty = true;
    }

    fn dismiss_user_warning(&mut self) {
        self.user_warning = None;
        self.dirty = true;
    }

    /// Persist metadata, surfacing read-only refusals to the user via an
    /// App-owned top-level warning banner. This is deliberately not routed
    /// through `InputBar::set_status`: event handling calls `sync_brain_status`
    /// after view updates, which can overwrite InputBar status labels before
    /// the user sees the warning.
    fn persist_metadata(&mut self, context: &'static str) -> bool {
        match self.metadata_store.save() {
            Ok(()) => true,
            Err(e) => {
                if e.downcast_ref::<ReadOnlyFutureSchema>().is_some() {
                    self.show_user_warning(format!(
                        "Read-only mode: session metadata was written by a newer SPUR. {context} not saved. Upgrade SPUR to enable writes."
                    ));
                } else {
                    tracing::warn!(error = %e, context, "failed to persist metadata");
                }
                false
            }
        }
    }

    /// Dispatch a crossterm event (keyboard, resize, mouse, etc.) to the active view.
    pub fn handle_crossterm_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                // Normalize macOS Option-key Unicode chars (e.g. `å` → Alt+a)
                // BEFORE any handler runs, so global chord checks like the
                // Alt+a Insights bypass see the resolved KeyEvent rather than
                // raw Option-character codepoints. View-level callers also
                // invoke this; the function is idempotent.
                let key = crate::views::normalize_macos_option(key);

                if self.record_panic_esc(key) {
                    return;
                }

                // Quit-confirm dialog takes priority: it captures every key.
                if self.quit_confirm_visible {
                    if is_quit_chord(key) {
                        self.confirm_quit();
                    } else {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                                self.confirm_quit();
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                self.quit_confirm_visible = false;
                            }
                            _ => {}
                        }
                    }
                    self.dirty = true;
                    return;
                }

                if self.collision_modal.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            self.collision_modal = None;
                        }
                        KeyCode::Char('N') | KeyCode::Char('n') => {
                            self.collision_modal = None;
                            self.process_action(Action::NewSessionRequested);
                        }
                        KeyCode::Char('P') | KeyCode::Char('p') => {
                            self.collision_modal = None;
                            self.process_action(Action::RequestSessions);
                        }
                        KeyCode::Enter => {
                            let acp = self
                                .collision_modal
                                .as_ref()
                                .map(|state| state.acp_id.clone());
                            self.collision_modal = None;
                            if let (Some(session_id), Some(tx)) = (acp, self.user_input_tx.as_ref())
                            {
                                let _ = tx.try_send(UserInput::ResumeSession { session_id });
                            }
                        }
                        _ => {}
                    }
                    self.dirty = true;
                    return;
                }

                // Ctrl+C / Ctrl+Q are the global quit chords. They run BEFORE
                // the upgrade-modal handler so the modal's `_ => swallow` arm
                // never eats a quit chord. First press opens the confirmation
                // prompt; pressing it again while the prompt is visible
                // bypasses confirmation and exits immediately.
                if is_quit_chord(key) {
                    self.request_quit();
                    return;
                }

                // Plan C Tier 2 — upgrade modal sits between Quit/Collision and
                // Help in the priority chain: a denial CTA demands user
                // attention so it preempts informational overlays, but defers
                // to Quit/Collision (which are already-in-progress user-driven
                // flows the modal would otherwise interrupt).
                if self.upgrade_modal.is_some() {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            self.upgrade_modal = None;
                        }
                        KeyCode::Char('s') => {
                            self.upgrade_modal = None;
                            self.show_user_warning(
                                "Run `spur auth status` in a shell to view tiers and license state."
                                    .into(),
                            );
                        }
                        KeyCode::Char('l') => {
                            self.upgrade_modal = None;
                            self.show_user_warning(
                                "Run `spur auth login --key <KEY>` in a shell to activate a license."
                                    .into(),
                            );
                        }
                        _ => { /* swallow other keys while the modal is up */ }
                    }
                    self.dirty = true;
                    return;
                }

                // Help overlay intercepts ? (toggle) and Esc (close) before views.
                if self.help_visible {
                    if self.is_undo_key(key) {
                        self.flash_hint_short("close help to undo");
                        return;
                    }
                    match key.code {
                        KeyCode::Char('?') | KeyCode::Esc => {
                            self.help_visible = false;
                            return;
                        }
                        _ => return, // swallow all keys while help is visible
                    }
                }

                // Priority 2.5 — palette overlay.
                if self.palette_visible {
                    match self.palette_state.handle_key(key) {
                        Some(PaletteIntent::Dismiss) => {
                            self.palette_visible = false;
                            self.palette_state.reset();
                            self.dirty = true;
                        }
                        Some(PaletteIntent::Accept(result)) => {
                            self.palette_visible = false;
                            self.palette_state.reset();
                            if let Some(action) = self.result_to_action(result) {
                                self.process_action(action);
                            }
                            self.dirty = true;
                        }
                        None => {
                            self.dirty = true;
                        }
                    }
                    return;
                }

                if !self.dashboard_tab_empty_deprecation_shown
                    && self.current_view == ViewId::Dashboard
                    && key.code == KeyCode::Tab
                    && key.modifiers.is_empty()
                    && self.dashboard.is_empty_root_input()
                {
                    self.flash_hint_short(DASHBOARD_TAB_DEPRECATION_HINT);
                    self.dashboard_tab_empty_deprecation_shown = true;
                }

                // Global Ctrl+K opens palette. Plain `:` is a Dashboard Navigate
                // alias only, so Compose mode can still type the character.
                let is_ctrl_k = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('k'));
                let is_dashboard_colon_alias = key.code == KeyCode::Char(':')
                    && key.modifiers.is_empty()
                    && self.current_view == ViewId::Dashboard
                    && self.dashboard.mode() == DashboardMode::Navigate;
                if is_ctrl_k || is_dashboard_colon_alias {
                    self.open_palette();
                    return;
                }

                if key.modifiers.contains(KeyModifiers::ALT)
                    && matches!(key.code, KeyCode::Char('a'))
                {
                    self.process_action(Action::OpenInsights);
                    return;
                }

                // === All overlay/modal/help/global-shortcut owners run above this line. ===
                // === Tombstone undo is the residual key-owner: fires only when no       ===
                // === narrower visible context wants u/Ctrl+Z.                            ===
                if self.is_undo_key(key) && self.handle_undo() {
                    self.dirty = true;
                    return;
                }

                let ctx = crate::views::ViewContext {
                    lineage: &self.lineage,
                    plan_projection: &self.plan_projection,
                    synopsis: &self.synopsis,
                    brain_status: &self.brain_status,
                    license_badge: self.license_badge.as_ref(),
                    flag_summary: self.flag_summary,
                    tombstone: None,
                    transient_hint_override: None,
                };
                let action = match self.current_view {
                    ViewId::Dashboard => self.dashboard.handle_key_with_worker_streams(
                        key,
                        &self.lineage,
                        &mut self.worker_streams,
                    ),
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.handle_key(key, &ctx)
                        } else {
                            None
                        }
                    }
                    ViewId::SessionPicker => self
                        .session_picker
                        .as_mut()
                        .and_then(|p| p.handle_key(key, &ctx)),
                    ViewId::PlanInspector(_) => self
                        .plan_inspector
                        .as_mut()
                        .and_then(|view| view.handle_key(key, &ctx)),
                    ViewId::IssueBrowser => self
                        .issue_browser
                        .as_mut()
                        .and_then(|view| view.handle_key(key, &ctx)),
                    #[cfg(feature = "analytics")]
                    ViewId::Insights => {
                        if let Some(view) = self.insights_view.as_mut() {
                            view.handle_key(key, &ctx)
                        } else if self.insights_init.is_some() {
                            // Init still running. Allow Esc to bail back
                            // to Dashboard; the background task continues
                            // and its result lands on the next tick.
                            match key.code {
                                KeyCode::Esc => Some(Action::NavigateBack),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    }
                    #[cfg(not(feature = "analytics"))]
                    ViewId::Insights => None,
                    #[cfg(feature = "markdown")]
                    ViewId::MermaidOverlay(_) => {
                        if let Some(viewer) = self.mermaid_viewer.as_mut() {
                            match key.code {
                                KeyCode::Char('[') | KeyCode::Char(']') => {
                                    if let Some(detail) = self.session_detail.as_ref() {
                                        let entries: Vec<_> = detail
                                            .mermaid_registry
                                            .iter()
                                            .map(|(k, v)| (*k, v))
                                            .collect();
                                        viewer.cycle(&entries, key.code == KeyCode::Char(']'));
                                        self.dirty = true;
                                    }
                                    None
                                }
                                _ => viewer.handle_key(key, &ctx),
                            }
                        } else {
                            None
                        }
                    }
                };
                let should_dismiss_warning = matches!(key.code, KeyCode::Esc)
                    && self.user_warning.is_some()
                    // SessionPicker treats Esc as exit-to-Dashboard, not NavigateBack.
                    && matches!(
                        action,
                        Some(Action::NavigateBack)
                            | Some(Action::NavigateTo(ViewId::Dashboard))
                    );

                if should_dismiss_warning {
                    self.dismiss_user_warning();
                } else if let Some(action) = action {
                    self.process_action(action);
                }
                self.dirty = true;
            }
            Event::Mouse(mouse) => {
                self.handle_mouse_event(mouse);
            }
            Event::Resize(_, _) => {
                #[cfg(feature = "markdown")]
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.invalidate_inline_protocols();
                }
                self.dirty = true;
            }
            Event::Paste(text) => {
                if self.quit_confirm_visible
                    || self.collision_modal.is_some()
                    || self.upgrade_modal.is_some()
                    || self.help_visible
                    || self.palette_visible
                {
                    return;
                }

                // Normalize line endings once at the event boundary so every
                // view (dashboard, session_detail, session_picker) sees `\n`
                // separators regardless of clipboard origin.
                let normalized;
                let text: &str = if text.contains('\r') {
                    normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                    &normalized
                } else {
                    &text
                };

                match self.current_view {
                    ViewId::Dashboard => self.dashboard.handle_paste(text),
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.handle_paste(text);
                        }
                    }
                    ViewId::SessionPicker => {
                        if let Some(ref mut picker) = self.session_picker {
                            picker.handle_paste(text);
                        }
                    }
                    ViewId::PlanInspector(_) => {}
                    ViewId::IssueBrowser => {}
                    ViewId::Insights => {}
                    #[cfg(feature = "markdown")]
                    ViewId::MermaidOverlay(_) => {}
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn record_panic_esc(&mut self, key: KeyEvent) -> bool {
        if key.code != KeyCode::Esc {
            return false;
        }

        let now = Instant::now();
        self.esc_chain.push_back(now);
        while self
            .esc_chain
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > PANIC_RESET_ESC_WINDOW)
        {
            self.esc_chain.pop_front();
        }
        while self.esc_chain.len() > 3 {
            self.esc_chain.pop_front();
        }

        if self.esc_chain.len() == 3 {
            self.process_action(Action::PanicReset);
            return true;
        }

        false
    }

    /// `u` is the view-level undo key. Ctrl+Z is only claimed in Emacs mode;
    /// Vim users keep Ctrl+Z available to their terminal conventions.
    fn is_undo_key(&self, key: KeyEvent) -> bool {
        let bare_u = key.code == KeyCode::Char('u') && key.modifiers.is_empty();
        let emacs_ctrl_z = key.code == KeyCode::Char('z')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !matches!(self.edit_mode, EditMode::Vim(_));
        bare_u || emacs_ctrl_z
    }

    /// Undo handler for `u` and Emacs Ctrl+Z.
    ///
    /// Returns `true` when the app consumed or explicitly blocked the key.
    /// Returns `false` when a narrower owner, such as the composer or picker,
    /// should receive the key unchanged.
    fn handle_undo(&mut self) -> bool {
        if self.input_bar_active_non_empty() {
            return false;
        }
        if self.picker_or_history_active() {
            return false;
        }
        if self.view_text_input_active() {
            return false;
        }
        if self.pending_permission.is_some() {
            return false;
        }
        if self.mermaid_render_picker_active() {
            return false;
        }

        let view = self.current_view.clone();
        let Some(tombstone) = self.tombstones.evict(&view) else {
            self.flash_hint_short("nothing to undo");
            return true;
        };

        match tombstone.kind {
            TombstoneKind::Reversible { inverse } => {
                self.flash_hint_short(format!("Undid: {}", tombstone.label));
                self.tombstone_undo_replay = true;
                self.process_action(inverse);
                self.tombstone_undo_replay = false;
            }
            TombstoneKind::QueuedRemote { pending: _ } => {
                self.flash_hint_short(format!("Cancelled: {}", tombstone.label));
            }
        }
        true
    }

    fn input_bar_active_non_empty(&self) -> bool {
        match &self.current_view {
            ViewId::Dashboard => self.dashboard.input_bar_active_non_empty(),
            ViewId::SessionDetail(_) => self
                .session_detail
                .as_ref()
                .is_some_and(SessionDetailView::input_bar_active_non_empty),
            _ => false,
        }
    }

    fn picker_or_history_active(&self) -> bool {
        match &self.current_view {
            ViewId::Dashboard => self.dashboard.completion_active(),
            ViewId::SessionDetail(_) => self
                .session_detail
                .as_ref()
                .is_some_and(SessionDetailView::completion_active),
            _ => false,
        }
    }

    fn view_text_input_active(&self) -> bool {
        matches!(self.current_view, ViewId::SessionPicker)
            && self.session_picker.as_ref().is_some_and(|picker| {
                picker.is_rename_active()
                    || picker.is_search_focused()
                    || picker.is_confirm_switch_visible()
            })
    }

    fn mermaid_render_picker_active(&self) -> bool {
        #[cfg(feature = "markdown")]
        {
            matches!(self.current_view, ViewId::MermaidOverlay(_)) && self.mermaid_viewer.is_some()
        }
        #[cfg(not(feature = "markdown"))]
        {
            false
        }
    }

    fn request_quit(&mut self) {
        self.quit_confirm_visible = true;
        self.dirty = true;
    }

    /// Render-gate predicate for the upgrade modal. The upgrade modal is
    /// suppressed whenever a higher-precedence modal (quit_confirm or
    /// collision) is up so on-screen visibility matches input precedence
    /// (quit_confirm > collision > upgrade).
    fn should_render_upgrade_modal(&self) -> bool {
        !self.quit_confirm_visible && self.collision_modal.is_none()
    }

    fn confirm_quit(&mut self) {
        // Flush any unsent draft to disk before we exit so the next
        // `spur watch` restores the latest text.
        self.force_flush_active_draft();
        self.quit_confirm_visible = false;
        self.should_quit = true;
        self.dirty = true;
    }

    /// Handle mouse scroll events. Only scroll wheel is processed —
    /// clicks and drags are ignored to avoid tmux/terminal conflicts.
    fn handle_mouse_event(&mut self, event: MouseEvent) {
        let lines: usize = match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        let is_up = matches!(event.kind, MouseEventKind::ScrollUp);

        match self.current_view {
            ViewId::Dashboard => {
                if self.dashboard.focused_node().is_some() {
                    if is_up {
                        self.dashboard.scroll_detail_up_by(lines);
                    } else {
                        self.dashboard.scroll_detail_down_by(lines);
                    }
                } else if is_up {
                    self.dashboard.scroll_activity_up_by(lines);
                } else {
                    self.dashboard.scroll_activity_down_by(lines);
                }
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    if is_up {
                        detail.scroll_up_by(lines);
                    } else {
                        detail.scroll_down_by(lines);
                    }
                }
            }
            ViewId::SessionPicker => {
                // No mouse scroll in v1 picker.
            }
            ViewId::PlanInspector(_) => {}
            ViewId::IssueBrowser => {
                if let Some(ref mut browser) = self.issue_browser {
                    if browser.issue_detail_visible() {
                        if is_up {
                            browser.scroll_issue_detail_up_by(lines as u16);
                        } else {
                            browser.scroll_issue_detail_down_by(lines as u16);
                        }
                    } else {
                        let count = browser.tracked_issues().len();
                        if count > 0 {
                            if is_up {
                                browser.issues_panel_mut().select_prev(lines, count);
                            } else {
                                browser.issues_panel_mut().select_next(lines, count);
                            }
                        }
                    }
                }
            }
            ViewId::Insights => {}
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => {}
        }
        self.dirty = true;
    }

    /// Forward a SpurEvent to all views that need it.
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        // Always fold into the lineage projection first. The projection is a
        // pure function of the event stream — view code reads from it later.
        self.lineage.apply(&event);
        #[cfg(feature = "analytics")]
        self.sync_live_cost_active_sessions();
        self.plan_projection.apply(&event);
        self.synopsis.apply(&event);

        // Route worker stream updates into per-executor ReactTraces.
        // Orphan drop: skip events whose executor the lineage doesn't
        // know yet, to avoid materializing a trace with AgentKind::Generic
        // that would never be corrected. Matches the brain view's fidelity
        // ceiling (events before SessionDetailView construction are lost).
        if let spur_acp::domain::events::SpurEventBody::WorkerNotification {
            executor_id,
            notification,
            ..
        } = &event.body
        {
            let exec_id = spur_core::lineage::types::ExecutorId::new(executor_id);
            if let Some(node) = self.lineage.node(&exec_id) {
                let agent_name = node.agent.clone();
                self.worker_streams
                    .route(executor_id, &agent_name, &notification.update);
            } else {
                tracing::trace!(
                    executor_id = %executor_id,
                    "dropping WorkerNotification for unknown executor (orphan)"
                );
            }
        }

        // Seed the per-executor trace from its stream_buffer on spawn.
        // For a fresh live ExecutorSpawned the buffer is empty (harmless no-op).
        // On replay the buffer may already be populated from subsequent replayed
        // events, so the Stream tab has content for pre-existing executors before
        // new WorkerNotifications arrive. One-time per executor — subsequent
        // WorkerNotification events append on top of the seeded entries.
        if let spur_acp::domain::events::SpurEventBody::ExecutorSpawned { id, .. } = &event.body {
            let exec_id = spur_core::lineage::types::ExecutorId::new(id);
            if let Some(node) = self.lineage.node(&exec_id) {
                let agent = node.agent.clone();
                let entries: Vec<_> = node.stream_buffer.iter().cloned().collect();
                self.worker_streams
                    .seed_from_stream_buffer(id, &agent, entries.iter());
            }
        }

        // Reset per-executor trace on retry. Mirrors the lineage
        // projection's `node.stream_buffer.clear()` on the same event.
        if let spur_acp::domain::events::SpurEventBody::ExecutorRetryStarted { id, .. } =
            &event.body
        {
            self.worker_streams.reset(id);
        }

        self.dirty = true;

        // Handle session list responses before forwarding to views
        match &event.body {
            SpurEventBody::SessionsListed { agent, sessions } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_sessions(agent.clone(), sessions.clone(), &self.synopsis);
                }
                return;
            }
            SpurEventBody::SessionsListError { message } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_error(message.clone());
                }
                return;
            }
            SpurEventBody::AuthRequired { session, message } => {
                if let Some(ref mut detail) = self.session_detail {
                    // Apply when the event matches the focused session OR when
                    // the event carries a sentinel/empty session id (spawn-side
                    // failures that happen before a session id is allocated).
                    let matches_focused = session.0 == detail.session_id().0;
                    let is_sentinel =
                        session.0.is_empty() || session.0 == "00000000-0000-0000-0000-000000000000";
                    if matches_focused || is_sentinel {
                        detail.auth_error = Some(message.clone());
                    } else {
                        tracing::trace!(
                            event_session = %session.0,
                            focused_session = %detail.session_id().0,
                            "AuthRequired for non-focused session; dropping"
                        );
                    }
                } else {
                    tracing::trace!("AuthRequired received but no session_detail focused");
                }
                return;
            }
            SpurEventBody::SessionHistory { entries, .. } => {
                tracing::info!(
                    entry_count = entries.len(),
                    has_session_detail = self.session_detail.is_some(),
                    "SessionHistory: replaying history"
                );
                if let Some(ref mut detail) = self.session_detail {
                    detail.replay_history(entries);
                    tracing::info!(
                        trace_entries = detail.trace_entry_count(),
                        "SessionHistory: replay complete"
                    );
                } else {
                    tracing::warn!("SessionHistory: session_detail is None, history lost!");
                }

                // Backfill global input history from replayed user messages
                // so Ctrl-P recalls past inputs even from older sessions.
                let mut changed = false;
                {
                    let hist = &mut self.metadata_store.metadata_mut().input_history;
                    for entry in entries {
                        if entry.role == "user" {
                            let history_entry = InputHistoryEntry::from_text(entry.text.clone());
                            changed |= Self::merge_input_history_entry(hist, history_entry);
                        }
                    }
                }
                if changed {
                    self.persist_metadata("backfilled input history");
                    self.sync_input_history();
                }

                return;
            }
            // Variants outside the session-list / auth pre-routing surface.
            // They flow through to the brain-status match below and the view
            // fan-out at the end of `handle_spur_event`. Logged at debug so
            // that a future variant added to `SpurEventBody` without a
            // routing decision is visible (R3: observability requires
            // explicitness — see docs/architecture.md §Risk Register #3).
            _ => {
                tracing::debug!(
                    seq = event.seq,
                    "SpurEventBody not pre-routed by session-list match; deferring to brain-status match + view fan-out"
                );
            }
        }

        // Track brain status transitions
        match &event.body {
            SpurEventBody::BrainConnectStarted { brain } => {
                self.brain_status = BrainStatus::Connecting;
                self.brain_name = Some(brain.clone());
            }
            SpurEventBody::BrainConnected { brain } => {
                self.brain_status = BrainStatus::Connected;
                self.brain_name = Some(brain.clone());
            }
            SpurEventBody::BrainConnectFailed { brain, reason } => {
                self.brain_status = BrainStatus::Error(reason.clone());
                self.brain_name = Some(brain.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainSpawned { agent, session } => {
                self.brain_status = BrainStatus::Thinking;
                self.brain_name = Some(agent.clone());
                self.sync_dashboard_workers();

                // Only create a new SessionDetailView if none exists or the
                // session ID changed. Replacing unconditionally would wipe any
                // user message that was just pushed to the trace.
                let needs_new = match &self.session_detail {
                    Some(detail) => detail.session_id() != session,
                    None => true,
                };
                if needs_new {
                    // Carry-over: a cleared view's InputBar text belongs to the NEW
                    // session, not the retired one. Capture owned text before
                    // dropping the old view. Source-level gating in
                    // force_save_draft / draft_save_action (spec §3.5) means
                    // force_flush_active_draft is a no-op for a cleared view, so
                    // no call-site gating is required here.
                    let carryover: Option<String> = self
                        .session_detail
                        .as_ref()
                        .filter(|d| d.is_cleared())
                        .map(|d| d.input_bar_text());
                    tracing::debug!(
                        carryover_len = carryover.as_deref().map(str::len).unwrap_or(0),
                        "view-replacement: clear-carryover capture"
                    );
                    self.force_flush_active_draft();

                    let agent_cfg = self.resolve_agent_config(agent);
                    let mut view = SessionDetailView::new(
                        session.clone(),
                        agent.clone(),
                        "brain".to_string(),
                        std::env::current_dir().unwrap_or_default(),
                        agent_cfg,
                        self.build_worker_snapshot(),
                    );
                    #[cfg(feature = "markdown")]
                    view.set_render_picker(self.mermaid_picker.clone());
                    view.seed_input_history(self.metadata_store.metadata().input_history.clone());
                    if let Some(entry) = self.metadata_store.entry(&session.0) {
                        view.restore_draft(&entry.draft);
                    }
                    // Carry-over wins over any metadata draft (which is normally
                    // empty for a freshly-minted spur_session_id anyway).
                    // restore_draft is a no-op on empty input.
                    if let Some(text) = carryover.as_deref() {
                        view.restore_draft(text);
                    }
                    // Auto-resume banner — unchanged from the pre-revision branch.
                    if self
                        .metadata_store
                        .metadata()
                        .last_active_session_id
                        .as_deref()
                        == Some(session.0.as_str())
                    {
                        let title = self
                            .metadata_store
                            .entry(&session.0)
                            .and_then(|e| e.title_override.clone())
                            .unwrap_or_else(|| agent.clone());
                        let quit_ago = humanize_since(
                            self.metadata_store.metadata().last_active_at.as_deref(),
                        );
                        view.show_resume_banner(title, quit_ago);
                        self.metadata_store.clear_last_active();
                        self.persist_metadata("cleared last_active");
                    }
                    self.session_detail = Some(view);
                }

                // Sync edit mode to newly created session detail view.
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);
                    detail.set_disable_paste_burst(self.config.tui.disable_paste_burst);
                }

                // Auto-navigate from Dashboard or SessionPicker
                if matches!(self.current_view, ViewId::Dashboard | ViewId::SessionPicker) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
            }
            SpurEventBody::AgentSessionReady {
                session,
                acp_session_id,
                brain,
                resumed: _,
                cancel_mode: _,
                fs_unsafe: _,
                caps,
            } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id() == session {
                        detail.set_spur_agent_caps(caps.clone());
                    }
                }
                self.metadata_store
                    .set_acp_mapping(&session.0, acp_session_id, brain);
                self.persist_metadata("AgentSessionReady metadata");
            }
            SpurEventBody::SessionAttachRejected {
                acp_session_id,
                holder,
                fs_unsafe: _,
            } => {
                self.collision_modal = Some(CollisionModalState {
                    acp_id: acp_session_id.clone(),
                    holder: holder.clone(),
                });
                self.dirty = true;
            }
            SpurEventBody::AgentNotification { session: _, .. } => {
                // Transition Thinking → Streaming on first output
                if self.brain_status == BrainStatus::Thinking {
                    self.brain_status = BrainStatus::Streaming;
                }
            }
            SpurEventBody::TurnComplete { session } => {
                self.brain_status = BrainStatus::Ready;
                let now = chrono::Utc::now().to_rfc3339();
                self.metadata_store.set_last_active(session.0.clone(), now);
                self.persist_metadata("last_active");
            }
            SpurEventBody::BrainError { message, .. } => {
                self.brain_status = BrainStatus::Error(message.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainReconnecting { .. } => {
                self.brain_status = BrainStatus::Thinking;
            }
            SpurEventBody::BrainReconnected { .. } => {
                self.brain_status = BrainStatus::Ready;
            }
            SpurEventBody::BrainReconnectFailed { reason, .. } => {
                self.brain_status = BrainStatus::Error(reason.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::SessionCompleted { .. } => {
                self.brain_status = BrainStatus::Idle;
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainRetired { reason, .. } => {
                // Null per-App state that was tied to the retired session.
                // `brain_status` is intentionally NOT touched here:
                //  - UserClear: already set to Idle by the ClearSession
                //    action handler before the event round-trips back.
                //  - ResumeSwitch: the orchestrator's ResumeSession arm
                //    is already loading the next brain; overriding to
                //    Idle would race that transition.
                self.brain_name = None;
                self.pending_first_user_message = None;
                // Clear auto-resume pointers so /clear followed by a
                // process quit before the next prompt does not cause
                // spur-cli to auto-resume the just-retired session on
                // the next launch. The next `AgentSessionReady` (on the
                // next prompt) repopulates these via `set_acp_mapping`.
                self.metadata_store.clear_last_active_full();
                self.persist_metadata("cleared last_active on BrainRetired");
                // Defensive belt-and-suspenders reset for the UserClear path.
                // Idempotent against Action::ClearSession's eager reset.
                // Gated on UserClear only:
                //  - ResumeSwitch: in-flight ResumeSession is already loading the next
                //    brain via BrainSpawned (app.rs:919-975); resetting here would
                //    briefly blank the new view mid-load.
                //  - Shutdown: terminal; reset is moot.
                if matches!(reason, BrainRetireReason::UserClear) {
                    tracing::info!("BrainRetired{{UserClear}}: defensive view reset");
                    if let Some(ref mut detail) = self.session_detail {
                        detail.reset_for_clear();
                    }
                }
            }
            SpurEventBody::LicenseUpdated { state } => {
                self.update_license_state(state.clone());
            }
            // Variants that don't affect brain status — handled by views.
            SpurEventBody::DelegationRequested { .. }
            | SpurEventBody::DelegationCompleted { .. }
            | SpurEventBody::DelegationDispatched { .. }
            | SpurEventBody::WorkerSpawned { .. }
            | SpurEventBody::WorkerNotification { .. }
            | SpurEventBody::WorkerProgress { .. }
            | SpurEventBody::WorkerFileTouched { .. }
            | SpurEventBody::WorkerHeartbeat { .. }
            | SpurEventBody::ExecutorPhaseChanged { .. }
            | SpurEventBody::ExecutorRetryStarted { .. }
            | SpurEventBody::ExecutorArtifact { .. }
            | SpurEventBody::ExecutorReviewRequested { .. }
            | SpurEventBody::ExecutorReviewResolved { .. }
            | SpurEventBody::ExecutorReviewCancelled { .. }
            | SpurEventBody::CostUpdate { .. }
            | SpurEventBody::ConflictDetected { .. }
            | SpurEventBody::RateLimitDetected { .. }
            | SpurEventBody::BrainFailover { .. }
            | SpurEventBody::IssueReceived { .. }
            | SpurEventBody::PrCreated { .. }
            | SpurEventBody::IssueUpdated { .. }
            | SpurEventBody::PlanSnapshotUpdated { .. }
            | SpurEventBody::AgentExtNotification { .. } => {}
            // Catch-all for future variants — log so we notice.
            _ => {
                tracing::debug!("unhandled SpurEventBody variant in brain status tracking");
            }
        }

        if let SpurEventBody::PromptDispatched {
            session, turn_kind, ..
        } = &event.body
        {
            let matches_active = self
                .session_detail
                .as_ref()
                .is_some_and(|detail| detail.session_id() == session);
            let should_drain = matches_active
                && matches!(turn_kind.as_str(), "user_only" | "merged")
                && self.session_detail.as_ref().is_some_and(|detail| {
                    // App handles this before SessionDetailView can add a merged-turn Think note.
                    detail.trace_entry_count() == 0
                });
            if should_drain {
                if let Some(message) = self.pending_first_user_message.take() {
                    if let Some(ref mut detail) = self.session_detail {
                        detail.append_user_message(&message);
                    }
                }
            }
        }

        // Forward to views
        let ctx = crate::views::ViewContext {
            lineage: &self.lineage,
            plan_projection: &self.plan_projection,
            synopsis: &self.synopsis,
            brain_status: &self.brain_status,
            license_badge: self.license_badge.as_ref(),
            flag_summary: self.flag_summary,
            tombstone: None,
            transient_hint_override: None,
        };
        self.dashboard.handle_spur_event(&event, &ctx);
        if let Some(ref mut picker) = self.session_picker {
            picker.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut detail) = self.session_detail {
            detail.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut inspector) = self.plan_inspector {
            inspector.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut browser) = self.issue_browser {
            browser.handle_spur_event(&event, &ctx);
        }

        // Sync status to InputBars
        self.sync_brain_status();
    }

    /// Process a single Action returned by a view.
    pub(crate) fn process_action(&mut self, action: Action) {
        #[cfg(any(test, debug_assertions))]
        {
            self.last_action = Some(action.clone());
        }
        match action {
            Action::Quit => {
                self.request_quit();
            }

            Action::NavigateTo(ViewId::SessionDetail(ref session_id)) => {
                if self.session_detail.is_some() {
                    // Just switch view — don't recreate. BrainSpawned is the only creator.
                    self.current_view = ViewId::SessionDetail(session_id.clone());
                }
                // If no session_detail exists (no brain spawned), ignore.
            }

            Action::NavigateTo(ViewId::Dashboard) => {
                self.current_view = ViewId::Dashboard;
                self.dirty = true;
                // session_detail kept alive (same as NavigateBack)
            }

            Action::NavigateTo(ViewId::SessionPicker) => {
                self.current_view = ViewId::SessionPicker;
            }

            Action::NavigateTo(ViewId::PlanInspector(ref session)) => {
                self.plan_inspector = Some(PlanInspectorView::new(session.clone()));
                self.current_view = ViewId::PlanInspector(session.clone());
                self.dirty = true;
            }

            Action::NavigateTo(ViewId::IssueBrowser) => {
                if self.issue_browser.is_none() {
                    self.issue_browser = Some(IssueBrowserView::new());
                }
                self.current_view = ViewId::IssueBrowser;
                self.dirty = true;
            }

            Action::OpenInsights | Action::NavigateTo(ViewId::Insights) => {
                #[cfg(feature = "analytics")]
                self.start_insights_init();
                self.current_view = ViewId::Insights;
                self.dirty = true;
            }

            #[cfg(feature = "markdown")]
            Action::NavigateTo(ViewId::MermaidOverlay(ref session)) => {
                use crate::views::mermaid_viewer::MermaidViewerView;
                self.mermaid_viewer = Some(MermaidViewerView::new(session.clone()));
                self.current_view = ViewId::MermaidOverlay(session.clone());
                self.dirty = true;
            }

            Action::NavigateBack => {
                #[cfg(feature = "markdown")]
                if let ViewId::MermaidOverlay(ref session) = self.current_view {
                    self.current_view = ViewId::SessionDetail(session.clone());
                    self.mermaid_viewer = None;
                    self.dirty = true;
                    return;
                }
                if let ViewId::PlanInspector(ref session) = self.current_view {
                    self.current_view = ViewId::SessionDetail(session.clone());
                    self.plan_inspector = None;
                    self.dirty = true;
                    return;
                }
                if matches!(self.current_view, ViewId::IssueBrowser) {
                    self.current_view = ViewId::Dashboard;
                    self.dirty = true;
                    return;
                }
                // From Dashboard: if an active session exists, return to it
                // (the natural "back" from the activity log). Otherwise do
                // nothing — quitting is now an explicit Ctrl+C flow.
                if matches!(self.current_view, ViewId::Dashboard) {
                    if let Some(ref detail) = self.session_detail {
                        self.current_view = ViewId::SessionDetail(detail.session_id().clone());
                        self.dirty = true;
                    }
                    return;
                }
                // From SessionDetail (or any other view): go to Dashboard.
                self.current_view = ViewId::Dashboard;
                // Note: session_detail is intentionally kept alive so it
                // continues accumulating events while the Dashboard is shown.
            }

            Action::SendMessage {
                mut session,
                blocks,
                interrupt,
            } => {
                // Plan C Tier 2 — MVP gate-check site for the upgrade
                // modal. `Action::SendMessage` is the dominant interactive
                // command-execution path in the TUI (every prompt to an
                // attached brain flows through it), making it the natural
                // counterpart to the CLI's `spur exec` denial path that
                // Tier 1 wired into stderr.
                //
                // `cli_core_exec` is community-tier in the embedded
                // policy, so production users will not normally hit this
                // branch — the MVP demo path is
                // `SPUR_LICENSE_TEST_STRIP_KEYS=cli_core_exec`, mirroring
                // the binary smoke pattern from Tier 1.
                if let Err(err) = spur_license::require_feature(
                    &self.feature_gate,
                    spur_license::FeatureKey::CLI_CORE_EXEC,
                ) {
                    let required_tier = spur_license::upgrade_cta::required_tier_for(
                        spur_license::FeatureKey::CLI_CORE_EXEC,
                    );
                    self.process_action(Action::ShowUpgradeModal { err, required_tier });
                    return;
                }

                // Empty session means "route to the currently active session".
                // Dashboard's InputBar emits this when a brain is attached.
                if session.0.is_empty() {
                    if let Some(ref detail) = self.session_detail {
                        session = detail.session_id().clone();
                    } else {
                        tracing::warn!(
                            "SendMessage with empty session and no active session_detail — \
                             dropping (caller should have used NewSessionWithMessage)"
                        );
                        return;
                    }
                }

                // Transition to Thinking when sending a message
                if matches!(
                    self.brain_status,
                    BrainStatus::Ready
                        | BrainStatus::Idle
                        | BrainStatus::Connected
                        | BrainStatus::Error(_)
                ) {
                    self.brain_status = BrainStatus::Thinking;
                }

                let preview = crate::commands::submit_router::blocks_preview(&blocks);

                tracing::info!(
                    text_len = preview.len(),
                    block_count = blocks.len(),
                    has_session_detail = self.session_detail.is_some(),
                    view = ?self.current_view,
                    brain_status = ?self.brain_status,
                    "SendMessage: pushing user message to trace"
                );

                // Add user message to Session Detail trace for instant feedback.
                // If session_detail doesn't exist yet, the caller should have
                // used NewSessionWithMessage; the dropped-message warning
                // above covers that path.
                if let Some(ref mut detail) = self.session_detail {
                    detail.push_user_message(&preview);
                    tracing::info!(
                        entries = detail.trace_entry_count(),
                        "SendMessage: pushed to session_detail"
                    );
                } else {
                    tracing::warn!(
                        "SendMessage: session_detail is None — no local echo (orchestrator owns the prompt)"
                    );
                }

                let history_entry = InputHistoryEntry::from_blocks(&blocks).with_context(
                    Some(chrono::Utc::now().to_rfc3339()),
                    Some(session.0.clone()),
                    self.brain_name.clone(),
                );

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput::Message {
                        session,
                        blocks,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
                }

                self.push_input_history_entry(history_entry);

                self.sync_brain_status();
            }

            Action::ClearSession => {
                self.pending_first_user_message = None;
                // /clear is a spur-local META command. Spec §3.6 requires
                // send-first ordering: if the channel send fails, the brain is
                // NOT retired, so we must NOT visually reset the view —
                // otherwise the user sees "cleared" while the stale brain is
                // still active (ghost-cleared state).
                let send_ok = match self.user_input_tx.as_ref() {
                    Some(tx) => match tx.try_send(UserInput::NewSessionWithMessage {
                        blocks: vec![],
                        interrupt: false,
                    }) {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::error!(
                                err = ?e,
                                "Action::ClearSession: user_input tx send failed — \
                                 brain NOT retired; view NOT reset to avoid ghost-cleared state"
                            );
                            false
                        }
                    },
                    None => {
                        tracing::error!(
                            "Action::ClearSession: user_input_tx is None; \
                             cannot retire brain — view NOT reset"
                        );
                        false
                    }
                };

                if send_ok {
                    self.brain_status = BrainStatus::Idle;
                    if let Some(ref mut detail) = self.session_detail {
                        detail.reset_for_clear();
                    }
                    self.sync_brain_status();
                    self.dirty = true;
                }
            }

            Action::NewSessionWithMessage { blocks, interrupt } => {
                // Transition to Thinking so the UI reflects work-in-flight
                // immediately; the orchestrator will spawn a brain and send
                // the prompt atomically.
                if matches!(
                    self.brain_status,
                    BrainStatus::Ready
                        | BrainStatus::Idle
                        | BrainStatus::Connected
                        | BrainStatus::Error(_)
                ) {
                    self.brain_status = BrainStatus::Thinking;
                }

                let preview = crate::commands::submit_router::blocks_preview(&blocks);
                self.pending_first_user_message = if blocks.is_empty() || preview.is_empty() {
                    None
                } else {
                    Some(preview)
                };

                let history_entry = InputHistoryEntry::from_blocks(&blocks).with_context(
                    Some(chrono::Utc::now().to_rfc3339()),
                    None,
                    self.brain_name.clone(),
                );
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::NewSessionWithMessage { blocks, interrupt });
                }
                self.push_input_history_entry(history_entry);
                self.sync_brain_status();
                self.dirty = true;
            }

            Action::VendorExec {
                session,
                method,
                params,
            } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::VendorExec {
                        session,
                        method,
                        params,
                    });
                }
            }

            Action::SetSessionConfigOption { config_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionConfigOption { config_id, value });
                }
            }

            Action::SetSessionModel { session_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionModel { session_id, value });
                }
            }

            Action::CancelStream { session } => {
                tracing::debug!(session = %session.0, "dispatching CancelStream to orchestrator");
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::CancelStream { session });
                }
            }

            Action::ShowUpgradeModal { err, required_tier } => {
                // Plan C Tier 2 — open the capability-tease modal.
                // Re-pop on every denial (no de-dup); the plan calls
                // out session-level suppression as YAGNI for the MVP.
                self.upgrade_modal = Some(UpgradeModalState { err, required_tier });
                self.dirty = true;
            }

            Action::InspectWorkers => {
                use crate::views::dashboard::Panel;
                use spur_acp::LifecycleState;
                // Pre-select: AwaitingReview > Running > most recent worker.
                let priority = self
                    .lineage
                    .nodes()
                    .filter(|n| n.role == spur_acp::Role::Executor)
                    .max_by_key(|n| match n.phase {
                        LifecycleState::AwaitingReview => 3,
                        LifecycleState::Running
                        | LifecycleState::Resuming
                        | LifecycleState::Spawning => 2,
                        _ => 1,
                    })
                    .map(|n| n.id.clone());
                self.dashboard.set_focused_panel(Panel::Agents);
                self.dashboard.set_focused_node(priority);
                self.current_view = ViewId::Dashboard;
                self.dirty = true;
            }

            Action::RequestSessions => {
                // Flush any unsent typing in the active SessionDetail into
                // metadata *before* the picker reads metadata to decide the
                // confirm-switch banner. Bypasses the 500ms debounce so text
                // typed within the debounce window is not lost on switch.
                self.force_flush_active_draft();
                // Retain the picker across opens so cursor + filter survive navigation.
                if self.session_picker.is_none() {
                    self.session_picker = Some(SessionPickerView::new());
                }
                self.refresh_picker_metadata();
                self.current_view = ViewId::SessionPicker;
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ListSessions);
                }
                self.dirty = true;
            }

            Action::ResumeSession { session_id } => {
                self.pending_first_user_message = None;
                // Optimistic navigation: move to SessionDetail immediately so
                // the picker dismisses in the same tick (FP-6). Lazy-construct
                // a pre-ready SessionDetailView so LoadState renders correctly
                // while the resume pipeline is in flight (Tranche 2 Task 5).
                let sid = SessionId(session_id.clone());
                self.session_detail =
                    Some(crate::views::session_detail::SessionDetailView::for_session(sid.clone()));
                self.current_view = ViewId::SessionDetail(sid);
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumeSession { session_id });
                }
            }

            Action::ToggleSessionPin { session_id } => {
                if !self.tombstone_undo_replay {
                    let will_pin = !self
                        .metadata_store
                        .entry(&session_id)
                        .is_some_and(|entry| entry.pinned);
                    let label = if will_pin {
                        format!("Pinned '{}'", session_id)
                    } else {
                        format!("Unpinned '{}'", session_id)
                    };
                    let now = Instant::now();
                    let inverse = Action::ToggleSessionPin {
                        session_id: session_id.clone(),
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    self.flash_hint(
                        format!("{} — press u to undo", label),
                        Duration::from_secs(2),
                    );
                }
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.pinned = !entry.pinned;
                self.persist_metadata("pin toggle");
                self.refresh_picker_metadata();
                self.dirty = true;
            }

            Action::ToggleSessionArchive {
                session_id,
                via_legacy_key,
            } => {
                let show_legacy_archive_hint = via_legacy_key && !self.legacy_archive_hint_shown;
                if show_legacy_archive_hint {
                    self.legacy_archive_hint_shown = true;
                }
                if !self.tombstone_undo_replay {
                    let will_archive = !self
                        .metadata_store
                        .entry(&session_id)
                        .is_some_and(|entry| entry.archived);
                    let label = if will_archive {
                        format!("Archived '{}'", session_id)
                    } else {
                        format!("Restored '{}'", session_id)
                    };
                    let now = Instant::now();
                    let inverse = Action::ToggleSessionArchive {
                        session_id: session_id.clone(),
                        via_legacy_key: false,
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    if !show_legacy_archive_hint {
                        self.flash_hint(
                            format!("{} — press u to undo", label),
                            Duration::from_secs(2),
                        );
                    }
                }
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.archived = !entry.archived;
                self.persist_metadata("archive toggle");
                self.refresh_picker_metadata();
                if show_legacy_archive_hint {
                    self.flash_hint_short(LEGACY_ARCHIVE_HINT);
                }
                self.dirty = true;
            }

            Action::ToggleShowArchived => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.toggle_show_archived(&self.synopsis);
                }
                self.dirty = true;
            }

            Action::RenameSession {
                ref session_id,
                ref new_title,
                ref original_title,
            } => {
                if !self.tombstone_undo_replay {
                    let label = format!("Renamed '{}' → '{}'", original_title, new_title);
                    let now = Instant::now();
                    let inverse = Action::RenameSession {
                        session_id: session_id.clone(),
                        new_title: original_title.clone(),
                        original_title: new_title.clone(),
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    self.flash_hint(
                        format!("{} — press u to undo", label),
                        Duration::from_secs(2),
                    );
                }
                let entry = self.metadata_store.entry_mut(session_id);
                entry.title_override = if new_title.trim().is_empty() {
                    None
                } else {
                    Some(new_title.clone())
                };
                self.persist_metadata("rename");
                self.refresh_picker_metadata();
                self.dirty = true;
            }

            Action::SaveDraft { session_id, draft } => {
                self.apply_save_draft(session_id, draft);
            }

            Action::RefreshSessions => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::ListSessions);
                }
                self.dirty = true;
            }

            Action::CopySessionId(session_id) => {
                use base64::{engine::general_purpose::STANDARD, Engine};
                use std::io::Write;
                let payload = STANDARD.encode(session_id.as_bytes());
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{payload}\x1b\\");
                let _ = out.flush();
                tracing::debug!(target: "spur_tui::picker", session_id = %session_id, "OSC 52 copy emitted");
            }

            Action::NewSessionRequested => {
                // Shut down the current brain atomically so picker [+ New session]
                // doesn't leave the old agent subprocess's session running.
                // Orchestrator's NewSessionWithMessage arm with empty blocks is
                // defined as "retire current brain, defer spawn to next Message."
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::NewSessionWithMessage {
                        blocks: vec![],
                        interrupt: false,
                    });
                }
                self.current_view = ViewId::Dashboard;
                self.dirty = true;
            }

            Action::TogglePlanMode => {
                // Cycle between "plan" and "default". If mode is unknown, assume
                // we're in "default" and jump to "plan".
                let current = self
                    .session_detail
                    .as_ref()
                    .and_then(|d| d.current_mode.as_deref());
                let next = match current {
                    Some("plan") => "default",
                    _ => "plan",
                };
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SetSessionMode {
                        mode_id: next.to_string(),
                    });
                }
                // Optimistic update so the status bar reflects the toggle
                // immediately; orchestrator will emit CurrentModeUpdate to
                // reconcile if the agent rejects the mode id.
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_current_mode(Some(next.to_string()));
                }
            }

            Action::ToggleVimMode => {
                self.edit_mode = match self.edit_mode {
                    EditMode::Emacs => EditMode::Vim(crate::components::input_bar::VimMode::Normal),
                    EditMode::Vim(_) => EditMode::Emacs,
                };
                self.dashboard.set_edit_mode(self.edit_mode);
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);

                    let configured = EditMode::from(self.config.tui.edit_mode);
                    if self.edit_mode != configured {
                        let label = match self.edit_mode {
                            EditMode::Emacs => "Emacs",
                            EditMode::Vim(_) => "Vim",
                        };
                        detail.push_persist_hint(label);
                    }
                }
                self.dirty = true;
            }

            Action::ToggleVerbose => {
                // Verbose mode is tracked by the dashboard view internally.
                // We toggle it via a dedicated method or re-send the key.
                // For now, the dashboard already handles this in handle_key.
            }

            Action::ShowHelp => {
                self.help_visible = true;
            }

            Action::HideHelp => {
                self.help_visible = false;
            }

            Action::PanicReset => {
                self.quit_confirm_visible = false;
                self.collision_modal = None;
                self.upgrade_modal = None;
                self.help_visible = false;
                self.palette_visible = false;
                self.palette_state.reset();
                self.tombstones.cancel_all_without_dispatch();
                // Wire per 2026-04-28-tui-destructive-undo-design.md §4.7.
                self.current_view = ViewId::Dashboard;
                self.dashboard.reset_to_root();
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.reset_to_root();
                }
                self.esc_chain.clear();
                self.flash_hint_short(PANIC_RESET_HINT);
                self.dirty = true;
            }

            Action::ShowSessionCost => {
                // M1.3 - Pro-tier demo gate: community users get the upgrade
                // modal; Pro users see the per-project cost view.
                if let Err(err) = spur_license::require_feature(
                    &self.feature_gate,
                    spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
                ) {
                    let required_tier = spur_license::upgrade_cta::required_tier_for(
                        spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
                    );
                    self.process_action(Action::ShowUpgradeModal { err, required_tier });
                    return;
                }

                if let Some(ref mut detail) = self.session_detail {
                    detail.push_cost_note();
                }
            }

            Action::PermissionGrant(choice) => {
                use crate::action::PermissionChoice;
                if let Some((perm, _)) = self.pending_permission.take() {
                    match choice {
                        PermissionChoice::Allow => {
                            let id = perm
                                .args
                                .options
                                .first()
                                .map(|o| o.option_id.to_string())
                                .unwrap_or_else(|| "allow".to_string());
                            let _ = perm
                                .reply_tx
                                .send(spur_acp::types::PermissionResponse { option_id: id });
                        }
                        PermissionChoice::AlwaysAllow => {
                            let id = perm
                                .args
                                .options
                                .iter()
                                .find(|o| o.name.to_lowercase().contains("always"))
                                .or(perm.args.options.first())
                                .map(|o| o.option_id.to_string())
                                .unwrap_or_else(|| "allow".to_string());
                            let _ = perm
                                .reply_tx
                                .send(spur_acp::types::PermissionResponse { option_id: id });
                        }
                        PermissionChoice::Deny => {
                            // Drop reply_tx (signals denial to ACP thread)
                            drop(perm);
                        }
                    }
                }
                self.clear_pending_permission_trace();
            }

            Action::SelectNextBy(n) => {
                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_next(&self.lineage);
                }
            }
            Action::SelectPrevBy(n) => {
                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_prev(&self.lineage);
                }
            }
            Action::FocusNode => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.set_focused_node(Some(id));
                }
            }
            Action::UnfocusNode => {
                self.dashboard.set_focused_node(None);
            }
            Action::JumpToReview => {
                // Cycle forward through pending reviews in DISPLAY order
                // (newest first), so `r`/`N` flows top-to-bottom on screen
                // matching the AgentsTree visual ordering.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let next = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| reviews.get(i + 1).cloned())
                    .or_else(|| reviews.into_iter().next());
                if let Some(id) = next {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
            Action::JumpToPreviousReview => {
                // Cycle backward through pending reviews in DISPLAY order
                // (newest first); "previous" means visually upward on screen.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let prev = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| i.checked_sub(1).and_then(|j| reviews.get(j).cloned()))
                    .or_else(|| reviews.last().cloned());
                if let Some(id) = prev {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
            Action::ToggleCollapse => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.agents_tree_mut().toggle_collapsed(&id);
                }
            }
            Action::SubmitReview {
                executor_id,
                attempt_n,
                decision,
            } => {
                let has_review = self
                    .lineage
                    .node(&spur_core::ExecutorId(executor_id.clone()))
                    .map(|n| n.pending_review.is_some())
                    .unwrap_or(false);
                if !has_review {
                    tracing::warn!(executor_id = %executor_id, "SubmitReview ignored: no pending review on this node");
                    return;
                }
                let decision_label = format!("{decision:?}");
                let label = format!("{decision_label}…");
                let pending_dispatch = Action::SubmitReviewDispatch {
                    executor_id: executor_id.clone(),
                    attempt_n,
                    decision,
                };
                let now = Instant::now();
                let displaced = self.tombstones.install_and_get_displaced(Tombstone {
                    view: ViewId::Dashboard,
                    kind: TombstoneKind::QueuedRemote {
                        pending: pending_dispatch,
                    },
                    label: label.clone(),
                    created_at: now,
                    expires_at: now + Duration::from_secs(3),
                });
                if let Some(displaced_ts) = displaced {
                    if let TombstoneKind::QueuedRemote { pending } = displaced_ts.kind {
                        self.process_action(pending);
                    }
                }
                self.flash_hint(
                    format!("{label} — press u to revert (3s)"),
                    Duration::from_secs(2),
                );
                self.dirty = true;
            }
            Action::SubmitReviewDispatch {
                executor_id,
                attempt_n,
                decision,
            } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SubmitReview {
                        executor_id: executor_id.clone(),
                        attempt_n,
                        decision: decision.clone(),
                    });
                }
                // Optimistically reflect the resolution locally so the UI
                // updates immediately without waiting for the authoritative
                // event to round-trip.
                self.lineage.apply(&spur_acp::SpurEvent::now(
                    spur_acp::SpurEventBody::ExecutorReviewResolved {
                        id: executor_id,
                        decision: to_wire_decision(&decision),
                    },
                ));
                self.flash_hint_short("Sent.");
                self.dirty = true;
                #[cfg(feature = "analytics")]
                self.sync_live_cost_active_sessions();
            }

            #[cfg(feature = "markdown")]
            Action::MermaidRenderRequest {
                session,
                ref_id,
                code,
                target_width,
            } => {
                let tx = self.mermaid_tx.clone();
                let session_cloned = session.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::components::mermaid::render_mermaid(&code, target_width)
                        .map(std::sync::Arc::new)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Action::MermaidRenderCompleted {
                        session: session_cloned,
                        ref_id,
                        target_width,
                        result,
                    });
                });
            }
            #[cfg(feature = "markdown")]
            Action::MermaidRenderCompleted {
                session,
                ref_id,
                target_width,
                result,
            } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id().0 == session.0 {
                        detail.handle_mermaid_completed(ref_id, target_width, result);
                    }
                }
                self.dirty = true;
            }

            // Scroll actions are already handled inside the views' handle_key methods.
            Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::CycleFocus
            | Action::Tick => {}

            // Issue actions — wired to the PM backend; IssuesPanel not yet implemented.
            Action::RefreshIssues => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshIssues);
                }
            }
            Action::Issue(issue_action) => {
                match issue_action {
                    crate::action::IssueAction::ViewDetail { id } => {
                        if let Some(ref tx) = self.user_input_tx {
                            let _ = tx.try_send(UserInput::GetIssueDetail { id });
                        }
                    }
                    crate::action::IssueAction::UpdateStatus {
                        id,
                        status,
                        via_legacy_key,
                    } => {
                        let show_legacy_close_hint = via_legacy_key
                            && status == "closed"
                            && !self.legacy_issue_close_hint_shown;
                        if show_legacy_close_hint {
                            self.legacy_issue_close_hint_shown = true;
                        }
                        if !self.tombstone_undo_replay {
                            let previous_status = self.issue_browser.as_ref().and_then(|view| {
                                view.tracked_issues()
                                    .iter()
                                    .find(|issue| issue.id.as_str() == id.as_str())
                                    .map(|issue| issue.status.clone())
                            });

                            if let Some(previous_status) = previous_status {
                                let label = format!("Issue '{}' → {}", id, status);
                                let now = Instant::now();
                                let inverse =
                                    Action::Issue(crate::action::IssueAction::UpdateStatus {
                                        id: id.clone(),
                                        status: previous_status,
                                        via_legacy_key: false,
                                    });
                                self.tombstones.install(Tombstone {
                                    view: ViewId::IssueBrowser,
                                    kind: TombstoneKind::Reversible { inverse },
                                    label: label.clone(),
                                    created_at: now,
                                    expires_at: now + Duration::from_secs(60),
                                });
                                if !show_legacy_close_hint {
                                    self.flash_hint(
                                        format!("{} — press u to undo", label),
                                        Duration::from_secs(2),
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    issue_id = %id,
                                    "issue not in tracked_issues; skipping tombstone install (undo unavailable for this update)"
                                );
                            }
                        }
                        if let Some(ref tx) = self.user_input_tx {
                            let _ = tx.try_send(UserInput::UpdateIssue {
                                id,
                                update: spur_pm::IssueUpdate {
                                    status: Some(status),
                                    ..Default::default()
                                },
                            });
                        }
                        if show_legacy_close_hint {
                            self.flash_hint_short(LEGACY_CLOSE_HINT);
                        }
                    }
                    crate::action::IssueAction::WorkOn { id } => {
                        // Construct issue prompt from cached summary
                        let prompt = if let Some(issue) =
                            self.dashboard.tracked_issues().iter().find(|i| i.id == id)
                        {
                            let pri = issue
                                .priority
                                .map(|p| format!("P{}", p))
                                .unwrap_or_default();
                            let itype = issue.issue_type.as_deref().unwrap_or("task");
                            format!(
                                "Work on this issue:\n\n\
                                 Issue: {} \u{2014} {}\n\
                                 Priority: {} | Type: {} | Status: {}\n\n\
                                 Use `get_issue` tool to read full details if needed.\n\
                                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.\n\
                                 Update issue status as you progress.",
                                id, issue.title, pri, itype, issue.status, id,
                            )
                        } else {
                            format!(
                                "Work on issue {}.\n\n\
                                 Use `get_issue` tool to read full details.\n\
                                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.",
                                id, id,
                            )
                        };

                        let blocks = vec![spur_acp::ContentBlock::Text(
                            spur_acp::TextContent::new(prompt),
                        )];

                        if self.session_detail.is_some() {
                            self.process_action(Action::SendMessage {
                                session: spur_acp::SessionId(String::new()),
                                blocks,
                                interrupt: false,
                            });
                        } else {
                            self.process_action(Action::NewSessionWithMessage {
                                blocks,
                                interrupt: false,
                            });
                        }
                    }
                }
            }
        }
    }

    fn handle_permission_request(&mut self, request: spur_acp::types::PermissionRequest) {
        // Auto-deny any existing pending permission (drops old reply_tx)
        self.pending_permission.take();

        // Extract description from SDK args
        let description = request
            .args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Tool call".to_string());

        // Push permission entry to the active session's trace
        if let Some(ref mut detail) = self.session_detail {
            detail.push_permission(&description, 30);
        }

        // Store with deadline
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        self.pending_permission = Some((request, deadline));
        self.dirty = true;
    }

    /// Mark all pending permission trace entries as resolved.
    fn clear_pending_permission_trace(&mut self) {
        if let Some(ref mut detail) = self.session_detail {
            detail.resolve_pending_permissions();
        }
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

    pub fn current_view(&self) -> &ViewId {
        &self.current_view
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
            ViewId::IssueBrowser => {
                if let Some(view) = self.issue_browser.as_mut() {
                    view.tick();
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
                    render_insights_init_placeholder(frame, view_area, state.started_at);
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

fn render_user_warning(frame: &mut Frame, area: ratatui::layout::Rect, message: &str) {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::Line,
        widgets::{Clear, Paragraph},
    };

    if area.width == 0 || area.height == 0 {
        return;
    }

    let text = Line::styled(
        ellipsize_for_width(message, area.width),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().bg(Color::Yellow)),
        area,
    );
}

fn ellipsize_for_width(message: &str, width: u16) -> String {
    let width = usize::from(width);
    let char_count = message.chars().count();
    if char_count <= width {
        return message.to_string();
    }
    if width <= 3 {
        return ".".repeat(width);
    }

    let mut text = message.chars().take(width - 3).collect::<String>();
    text.push_str("...");
    text
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
    )
    .await
}

pub async fn run_tui_with_license(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker_with_preselect: Option<Option<String>>,
    config: std::sync::Arc<spur_acp::SpurConfig>,
    license_state: LicenseStateEvent,
    landing: crate::landing::LandingDecision,
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::build_with_license_state(
        user_input_tx,
        start_in_picker_with_preselect,
        config.clone(),
        license_state,
        landing,
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
) -> anyhow::Result<()> {
    run_tui_with_license(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker.then_some(None),
        config,
        App::default_license_state(PLACEHOLDER_STATUS_TEXT),
        crate::landing::LandingDecision::ShowDashboard,
    )
    .await
}

// ─── Free helpers ──────────────────────────────────────────────────────

/// Apply read-only session-scoped `SessionUpdate` variants to a
/// `SessionDetailView`. Variants not handled here are intentionally left to
/// the trace-rendering code in `session_detail::handle_spur_event`. Unknown
/// variants log at TRACE so future protocol additions don't crash the UI.
pub(crate) fn apply_session_update(
    state: &mut SessionDetailView,
    update: &spur_acp::SessionUpdate,
) {
    use spur_acp::SessionUpdate::*;
    match update {
        CurrentModeUpdate(u) => {
            state.set_current_mode(Some(u.current_mode_id.to_string()));
        }
        AvailableCommandsUpdate(u) => {
            state.apply_available_commands(&u.available_commands);
        }
        ConfigOptionUpdate(u) => {
            // Mid-session refresh: agent advertises a new snapshot of
            // session config options (e.g. external client mutated the
            // model/effort, or codex emits the post-load snapshot). Rebuild
            // synthesized advertised commands and refresh the cached
            // snapshot so any open SlashArg picker shows live choices.
            state.apply_advertised_commands(&u.config_options);
        }
        UsageUpdate(u) => {
            state.context_used = Some(u.used);
            state.context_size = Some(u.size);
        }
        SessionInfoUpdate(_) => {
            // M9 hoist: the cached title / updated_at moved to
            // BrainSession in spur-core. Wire-side ingestion of this
            // notification onto the orchestrator entry is tracked as
            // a follow-up; the explicit arm stays here so the variant
            // is still tagged in trace logs (vs. the catch-all silent
            // drop in `apply_session_update: unhandled variant`).
            tracing::trace!(
                "SessionInfoUpdate received in spur-tui — orchestrator-side ingestion is the canonical path post-M9"
            );
        }
        _ => {
            tracing::trace!("apply_session_update: unhandled variant");
        }
    }
}

fn to_wire_decision(d: &spur_core::ReviewDecision) -> spur_acp::ReviewDecision {
    d.clone()
}

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
