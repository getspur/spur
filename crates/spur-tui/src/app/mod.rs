//! App orchestration root. This module defines the shared `App` state plus the
//! render and tick loops. Submodules own focused `impl App` blocks:
//! `session` handles session construction, draft persistence, input history,
//! user-input channels, and brain status synchronization; `input`, `events`,
//! `navigation`, `overlays`, `analytics`, and `action_routing` own their named
//! runtime surfaces; `test_utils` and `tests` contain white-box test support.

mod action_routing;
mod analytics;
mod events;
mod input;
mod navigation;
mod overlays;
mod session;
mod test_utils;
#[cfg(test)]
mod tests;

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
