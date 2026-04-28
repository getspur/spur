use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use spur_acp::{SessionId, SpurEvent, SpurEventBody};

use crate::action::{Action, ViewId};
use crate::components::input_bar::{ActivityKind, EditMode, InputBar};
use crate::components::react_trace::{ReactTrace, TraceEntry, TraceKind};
use crate::components::status_bar::{StatusBar, StatusBarProps};
use crate::input_history::InputHistoryEntry;

use super::View;

const READY_BANNER_TEXT: &str = "✨ Session cleared — your next prompt starts a fresh brain.";

/// Derived render state for a session the user has navigated to but
/// whose resume pipeline may not yet be complete. Each variant is a
/// projection of the most recent milestone event received for this
/// view's session id (FP-2, FP-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Default initial state when SessionDetail is entered via
    /// optimistic navigation from the picker.
    Retiring,
    Connecting {
        brain_name: String,
    },
    Loading,
    Ready,
    Failed {
        message: String,
    },
}

/// Full-screen view of a brain session's ReAct trace with chat input.
pub struct SessionDetailView {
    session_id: SessionId,
    agent_name: String,
    role: String,
    /// The AgentConfig backing this session. Owns the CommandsConfig used
    /// by the ingest/response loops, and the effective permissions.
    agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
    react_trace: ReactTrace,
    input_bar: InputBar,
    cost: f64,
    started_at: Instant,
    /// Current session mode id (e.g. "plan", "default"). Populated from
    /// `SessionUpdate::CurrentModeUpdate`.
    pub current_mode: Option<String>,
    /// Merged slash-command registry (spur-local + agent-advertised).
    /// Populated from `SessionUpdate::AvailableCommandsUpdate` for the
    /// agent portion.
    pub(crate) command_registry: crate::commands::CommandRegistry,
    /// Tokens currently used in the agent's context window. Populated from
    /// `SessionUpdate::UsageUpdate`.
    pub context_used: Option<u64>,
    /// Total context window size in tokens. Populated from
    /// `SessionUpdate::UsageUpdate`.
    pub context_size: Option<u64>,
    /// Most recent auth-required error for this session. Rendered as a red
    /// banner at the top of the view. Dismissed on the next keystroke.
    pub auth_error: Option<String>,
    /// Shared completion popup pipeline for @mentions and slash commands.
    completion: crate::components::input_completion::InputCompletionPort,
    /// Registry of `@`-mention sources (files, directories).
    mention_registry: std::rc::Rc<std::cell::RefCell<crate::mentions::MentionRegistry>>,
    /// Working directory used to resolve file mentions.
    cwd: std::path::PathBuf,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    /// Owns rendered protocols for diagrams in `mermaid_registry`. Sibling
    /// of the registry so we can split-borrow during render.
    #[cfg(feature = "markdown")]
    pub(crate) image_cache: crate::components::image_cache::ImageCache,
    /// Coalesces re-raster requests — at most one in flight per id.
    #[cfg(feature = "markdown")]
    pub(crate) in_flight_renders: std::collections::HashSet<
        crate::components::mermaid::MermaidId,
    >,
    /// Source of monotonic `image_generation` values stored on
    /// `MermaidState::Ready` and snapshotted by `image_cache` for
    /// stale-protocol detection.
    #[cfg(feature = "markdown")]
    pub(crate) next_image_generation: u64,
    #[cfg(feature = "markdown")]
    pub(crate) pending_fence_actions: std::collections::VecDeque<crate::action::Action>,
    /// Graphics `Picker` used to build inline mermaid image protocols during
    /// render. Set once from `App` when the view is created; `None` when no
    /// graphics protocol is available (text fallback kicks in).
    #[cfg(feature = "markdown")]
    pub(crate) render_picker: Option<ratatui_image::picker::Picker>,
    /// Timestamp of the most recent InputBar text change whose contents
    /// differ from `last_persisted_draft`. `None` while the debounce is idle.
    last_draft_change_at: Option<std::time::Instant>,
    /// Last InputBar text value written to the metadata store (initially "").
    last_persisted_draft: String,
    /// Informational banner shown when the session was auto-resumed on
    /// startup. Auto-fades after 3s or on first keystroke.
    resume_banner: Option<crate::components::resume_banner::ResumeBanner>,
    /// True from the first `AgentMessageChunk`/`AgentThoughtChunk` of a turn
    /// until the matching `TurnComplete`. Used to gate `Esc`-to-cancel on
    /// whether a stream is actually in flight, and to render the "Esc to
    /// stop" status-bar hint.
    pub(crate) stream_in_flight: bool,

    /// True from the moment we dispatch `Action::CancelStream` until
    /// `TurnComplete`. Overrides the streaming label with `cancelling…` and
    /// prevents re-entrant cancel dispatches (the next `Esc` falls through
    /// to existing handlers, e.g. NavigateBack).
    pub(crate) cancelling_in_flight: bool,

    /// How `AgentConnection::cancel` behaves for this session's transport.
    /// Populated from `SpurEventBody::AgentSessionReady`. Used to select
    /// transport-aware text for the cancel system note. `None` until
    /// `AgentSessionReady` arrives; in that window, a generic fallback is
    /// rendered.
    pub(crate) cancel_mode: Option<spur_acp::CancelMode>,
    /// True when the session attached without an enforceable filesystem lock.
    fs_unsafe: bool,

    /// Whether the inline workers panel is collapsed. Toggled by Alt+D.
    workers_panel_collapsed: bool,
    /// Maps ToolCall id -> render depth for subagent nesting.
    /// Populated on each ToolCall; read on subsequent ToolCalls to resolve
    /// the parent's depth. Capped at 8 to prevent runaway indentation.
    tool_depth: std::collections::HashMap<String, u8>,
    /// Set of known worker names, derived once at construction from
    /// the worker snapshot supplied to `new`. Used by
    /// `prepend_worker_hint` to filter unknown-name atoms out of
    /// the hint.
    known_worker_names: std::collections::HashSet<String>,

    /// True once this view has been reset by `/clear` and is waiting for
    /// the next `BrainSpawned` to be replaced. While `cleared`, the view's
    /// `session_id` is treated as opaque — `force_save_draft` and
    /// `draft_save_action` both return `None` early so no metadata write
    /// can target the retired session. See spec §3.5.
    cleared: bool,

    /// Transient banner rendered in the same layout slot as
    /// `resume_banner` when the view has been cleared. Cleared by
    /// construction of the next view (replacement drops it naturally).
    ready_banner: Option<String>,

    /// Derived load state for this session. Transitions from `Retiring`
    /// through `Connecting` → `Loading` → `Ready` as resume-pipeline
    /// milestone events arrive. Set to `Failed` on `BrainError`.
    /// Drives the pre-ready render path (Tranche 2 Task 5).
    pub load_state: LoadState,

    /// Most recent snapshot of advertised session config options for this
    /// session. Populated from `SpurEventBody::CommandRegistryDirty` (which
    /// the orchestrator emits at session creation and after each successful
    /// `set_session_config_option`). Drives both the synthesized `/model` and
    /// `/effort` slash entries in `command_registry` and the `SlashArg`
    /// picker's choice list via `CompletionEnv.session_config_options`.
    session_config_options: Vec<spur_acp::SessionConfigOption>,

    /// Wave B/C (M8): cached `SpurAgentCaps` for this session. Populated by
    /// the upstream wiring once `Orchestrator::spur_agent_caps()` returns
    /// `Some(_)` (M9 ties this to a `SpurEventBody` arm). When `None`,
    /// caps are absent (e.g. resumed sessions before M9 wires
    /// `LoadSessionResponse`); the registry filter and submit-router
    /// treat `None` as permissive — full capability set assumed (F-3).
    spur_agent_caps: Option<std::sync::Arc<spur_acp::SpurAgentCaps>>,
}

impl SessionDetailView {
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
        worker_snapshot: Vec<crate::mentions::WorkerMentionDescriptor>,
    ) -> Self {
        let command_registry =
            crate::commands::CommandRegistry::from_configs(std::slice::from_ref(&*agent_cfg));
        let agent_kind = agent_cfg.kind;
        let known_worker_names: std::collections::HashSet<String> =
            worker_snapshot.iter().map(|d| d.name.clone()).collect();
        let mention_registry = if role == "brain" {
            crate::mentions::MentionRegistry::for_brain_session(worker_snapshot.clone())
        } else {
            crate::mentions::MentionRegistry::for_direct_session()
        };
        Self {
            session_id,
            agent_name,
            role,
            agent_cfg,
            react_trace: ReactTrace::with_kind(agent_kind),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd,
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names,
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Ready,
            session_config_options: Vec::new(),
            spur_agent_caps: None,
        }
    }

    /// Test-only convenience constructor — wraps `new()` with sensible
    /// defaults so unit tests don't have to repeat the full argument list.
    #[cfg(any(test, debug_assertions))]
    pub fn new_for_tests() -> Self {
        Self::new(
            spur_acp::SessionId("test".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    /// Minimal `SessionDetailView` for palette-integration tests — only
    /// `command_registry` is meaningfully populated. Not suitable for tests
    /// that exercise render, input bar, or trace paths.
    ///
    /// MAINTENANCE: This is a manual struct literal because `SessionDetailView`
    /// holds types without a meaningful `Default` (`Instant::now()`,
    /// `Rc<RefCell<MentionRegistry>>`, `Arc<AgentConfig>`, `PathBuf`).
    /// **Every new field added to `SessionDetailView` must also be added
    /// here, otherwise palette tests will fail to compile.** Keep the field
    /// initializers in the same order as the struct definition for easy
    /// audit.
    #[cfg(any(test, debug_assertions))]
    pub fn new_for_palette_test(
        command_registry: crate::commands::registry::CommandRegistry,
    ) -> Self {
        let mention_registry = crate::mentions::MentionRegistry::for_direct_session();
        Self {
            session_id: spur_acp::SessionId("palette-test".into()),
            agent_name: "palette-test-agent".to_string(),
            role: "brain".to_string(),
            agent_cfg: std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(
                "palette-test-agent",
            )),
            react_trace: crate::components::react_trace::ReactTrace::with_kind(
                spur_acp::AgentKind::Generic,
            ),
            input_bar: crate::components::input_bar::InputBar::new(),
            cost: 0.0,
            started_at: std::time::Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: std::path::PathBuf::from("."),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names: std::collections::HashSet::new(),
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Ready,
            session_config_options: Vec::new(),
            spur_agent_caps: None,
        }
    }

    /// Construct a pre-ready `SessionDetailView` for a session that has been
    /// navigated to optimistically (before the resume pipeline completes).
    /// Starts in `LoadState::Retiring`. Transitions via `handle_spur_event`
    /// as milestone events arrive (Tranche 2 Task 5).
    pub fn for_session(session_id: SessionId) -> Self {
        let mention_registry = crate::mentions::MentionRegistry::for_direct_session();
        let agent_cfg = std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(""));
        let command_registry =
            crate::commands::CommandRegistry::from_configs(std::slice::from_ref(&*agent_cfg));
        Self {
            session_id,
            agent_name: String::new(),
            role: String::new(),
            agent_cfg,
            react_trace: crate::components::react_trace::ReactTrace::with_kind(
                spur_acp::AgentKind::Generic,
            ),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: std::time::Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: std::path::PathBuf::from("."),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names: std::collections::HashSet::new(),
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Retiring,
            session_config_options: Vec::new(),
            spur_agent_caps: None,
        }
    }

    /// Return the current `LoadState` for this view.
    pub fn load_state(&self) -> &LoadState {
        &self.load_state
    }

    /// Update `load_state` from a milestone event scoped to this view's
    /// session id. Intended for tests that exercise the pre-ready load
    /// pipeline without a full `ViewContext`.
    ///
    /// The full `View::handle_spur_event` trait method also calls
    /// `apply_milestone_event` internally; this is the test-facing entry
    /// point.
    #[cfg(any(test, debug_assertions))]
    pub fn apply_spur_event(&mut self, event: &SpurEvent) {
        self.apply_milestone_event(event);
    }

    /// Inner projection: update `load_state` from a milestone event scoped to
    /// this view's session id.
    fn apply_milestone_event(&mut self, event: &SpurEvent) {
        match &event.body {
            SpurEventBody::BrainConnecting {
                session,
                brain_name,
            } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Connecting {
                    brain_name: brain_name.clone(),
                };
            }
            SpurEventBody::SessionLoading { session } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Loading;
            }
            SpurEventBody::SessionLoaded { session } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Ready;
            }
            SpurEventBody::BrainError { session, message } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Failed {
                    message: message.clone(),
                };
            }
            _ => {}
        }
    }

    /// Show the resume banner for an auto-resumed session. Called by App on
    /// startup after reading session metadata.
    pub fn show_resume_banner(&mut self, title: String, quit_ago: String) {
        self.resume_banner = Some(crate::components::resume_banner::ResumeBanner::new(
            title, quit_ago,
        ));
    }

    /// Wipe conversation-scoped state in place so the same view can host
    /// the next prompt without reconstruction.
    ///
    /// Called by `Action::ClearSession` (eager, gated on a successful
    /// `tx.try_send`) and by the `BrainRetired{UserClear}` event arm
    /// (defensive, idempotent).
    ///
    /// # Classification policy
    ///
    /// Every field on `SessionDetailView` MUST have a deliberate
    /// classification here. When adding a new field, update this method
    /// AND update `SessionDetailView::new` / `new_for_palette_test` per
    /// the existing maintenance rule (see doc comment at line ~174).
    ///
    /// **Cleared** (reset to the empty/default value for that field):
    /// - Conversation: `react_trace` (content wiped; keeps its `AgentKind`
    ///   and `mermaid_enabled` config), `tool_depth`, `mermaid_registry`,
    ///   `pending_fence_actions`.
    /// - Header/status (Task 3): `cost`, `started_at`, `current_mode`
    ///   (plus `react_trace.set_mode(None)` — see §3.3 of spec), `context_used`,
    ///   `context_size`, `auth_error`.
    /// - Stream flags (Task 3): `stream_in_flight`, `cancelling_in_flight`.
    /// - UI transient: `resume_banner`, `completion`.
    /// - Draft debounce locals (Task 4): `last_persisted_draft`,
    ///   `last_draft_change_at`.
    /// - Marks set: `cleared = true`, `ready_banner = Some(...)`.
    ///
    /// **Preserved** (the view survives, only its conversation is wiped):
    /// - `session_id`, `agent_name`, `role`, `agent_cfg`, `cwd`,
    ///   `command_registry`, `mention_registry`, `input_bar`,
    ///   `cancel_mode`, `workers_panel_collapsed`, `known_worker_names`,
    ///   `render_picker` (mermaid picker).
    pub fn reset_for_clear(&mut self) {
        tracing::debug!(
            session = %self.session_id.0,
            "SessionDetailView::reset_for_clear"
        );
        // Conversation / caches.
        self.react_trace.clear();
        self.tool_depth.clear();
        #[cfg(feature = "markdown")]
        {
            self.invalidate_inline_protocols();
            self.mermaid_registry.clear();
            self.pending_fence_actions.clear();
        }
        self.completion.reset();
        self.resume_banner = None;

        // Header / status.
        self.cost = 0.0;
        self.started_at = std::time::Instant::now();
        self.current_mode = None;
        self.react_trace.set_mode(None); // mirror for pane-title badge (set_current_mode pattern)
        self.context_used = None;
        self.context_size = None;
        self.auth_error = None;
        // Stream flags.
        self.stream_in_flight = false;
        self.cancelling_in_flight = false;

        // Draft debounce locals (spec §3.5). Gate is ALSO at the source in
        // force_save_draft/draft_save_action — this local wipe is
        // belt-and-suspenders for the debounce's own state machine.
        self.last_persisted_draft.clear();
        self.last_draft_change_at = None;

        // Marks.
        self.cleared = true;
        self.ready_banner = Some(READY_BANNER_TEXT.to_string());
    }

    /// Whether the resume banner is currently visible (not dismissed and
    /// within its 3s auto-fade window).
    pub fn banner_is_visible(&self) -> bool {
        self.resume_banner
            .as_ref()
            .map(|b| b.should_render())
            .unwrap_or(false)
            || self.ready_banner.is_some()
    }

    /// Test-only: current banner state, if a resume banner is present.
    #[cfg(any(test, debug_assertions))]
    pub fn banner_state(&self) -> Option<crate::components::resume_banner::BannerState> {
        self.resume_banner.as_ref().map(|b| b.state())
    }

    /// Test/debug helper. Overrides the internal debounce clock so tests can
    /// simulate an elapsed debounce window without sleeping.
    pub fn test_set_last_draft_change(&mut self, at: std::time::Instant) {
        self.last_draft_change_at = Some(at);
    }

    /// If the InputBar text has changed and >= 500ms have elapsed since the
    /// last change, returns a `SaveDraft` action and arms the debounce for
    /// the next edit. Otherwise returns `None`. Called from `App`'s tick loop.
    pub fn draft_save_action(&mut self) -> Option<Action> {
        if self.cleared {
            self.last_draft_change_at = None;
            return None;
        }
        let at = self.last_draft_change_at?;
        if at.elapsed() < std::time::Duration::from_millis(500) {
            return None;
        }
        let current = self.input_bar.text().to_string();
        if current == self.last_persisted_draft {
            self.last_draft_change_at = None;
            return None;
        }
        self.last_persisted_draft = current.clone();
        self.last_draft_change_at = None;
        Some(Action::SaveDraft {
            session_id: self.session_id.0.clone(),
            draft: current,
        })
    }

    /// Synchronously flush the current InputBar text to a `SaveDraft` action
    /// regardless of whether the 500ms debounce has elapsed. Returns `None` if
    /// the draft is unchanged since the last persist (no-op). Callers must
    /// apply the returned action **same-tick** at user-intent boundaries
    /// (opening the picker, quit-confirm proceed, brain respawn with a
    /// different session id) so metadata reflects the latest on-screen text
    /// before any code reads it (e.g., the confirm-switch banner decision).
    pub fn force_save_draft(&mut self) -> Option<Action> {
        if self.cleared {
            // A cleared view's session_id is opaque; any SaveDraft keyed
            // on it would corrupt the retired session's metadata.
            // Carry-over into the next view happens in the App-side
            // replacement path via `restore_draft`. See spec §3.5.
            self.last_draft_change_at = None;
            return None;
        }
        let current = self.input_bar.text().to_string();
        if current == self.last_persisted_draft {
            self.last_draft_change_at = None;
            return None;
        }
        self.last_persisted_draft = current.clone();
        self.last_draft_change_at = None;
        Some(Action::SaveDraft {
            session_id: self.session_id.0.clone(),
            draft: current,
        })
    }

    /// Pre-fill the InputBar with a previously-saved draft. Empty string = no-op.
    /// Also marks the draft as persisted so the next debounce tick doesn't
    /// re-save the same text.
    pub fn restore_draft(&mut self, draft: &str) {
        if draft.is_empty() {
            return;
        }
        self.input_bar.set_text(draft.to_string(), draft.len());
        self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
        self.last_persisted_draft = draft.to_string();
    }

    /// Seed the InputBar with global input history (loaded from metadata).
    pub fn seed_input_history(&mut self, entries: Vec<InputHistoryEntry>) {
        self.input_bar.seed_history(entries);
    }

    /// Current text content of the InputBar (read-only accessor for tests).
    pub fn input_bar_text(&self) -> String {
        self.input_bar.text()
    }

    /// True once this view has been reset for `/clear` and is awaiting replacement.
    pub fn is_cleared(&self) -> bool {
        self.cleared
    }

    /// The ready-banner text for this view, if any.
    #[cfg(test)]
    pub fn ready_banner_text(&self) -> Option<&str> {
        self.ready_banner.as_deref()
    }

    /// True if this view currently carries a resume banner (not fade-state-aware).
    pub fn has_resume_banner(&self) -> bool {
        self.resume_banner.is_some()
    }

    /// Install the graphics `Picker` used to build inline mermaid protocols.
    /// Called by `App` once after view construction. Cheap clone of a small value.
    /// Also forwards the capability bit into `ReactTrace` so new per-entry
    /// streams render ```mermaid fences as plain code in text mode.
    #[cfg(feature = "markdown")]
    pub fn set_render_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
        let enabled = picker.is_some();
        self.render_picker = picker;
        self.react_trace.set_mermaid_enabled(enabled);
    }

    /// Whether a graphics picker is installed (and therefore Alt+V should
    /// open the mermaid overlay instead of falling through to the composer).
    #[cfg(feature = "markdown")]
    fn has_render_picker(&self) -> bool {
        self.render_picker.is_some()
    }

    /// No-op fallback when markdown feature is disabled.
    #[cfg(not(feature = "markdown"))]
    fn has_render_picker(&self) -> bool {
        false
    }

    /// Drop every cached protocol so they are rebuilt at the new Rect size
    /// on the next render. Called on terminal resize (app.rs:876) and on
    /// session reset.
    #[cfg(feature = "markdown")]
    pub fn invalidate_inline_protocols(&mut self) {
        self.image_cache.invalidate_all();
    }

    /// The session ID this view tracks.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Number of entries in the trace (for debug logging).
    pub fn trace_entry_count(&self) -> usize {
        self.react_trace.entry_count()
    }

    /// Expose the react trace for inspection (used in tests).
    pub fn react_trace(&self) -> &ReactTrace {
        &self.react_trace
    }

    /// Merged slash-command registry (spur-local + agent-advertised).
    pub fn command_registry(&self) -> &crate::commands::CommandRegistry {
        &self.command_registry
    }

    /// Lowercase agent identifier used for namespacing commands in the
    /// registry (e.g. `"claude"`, `"kiro"`).
    pub(crate) fn agent_handle_for_commands(&self) -> String {
        self.agent_cfg.effective_handle()
    }

    /// Handle an ACP `SessionUpdate::AvailableCommandsUpdate`. Builds
    /// `CommandEntry` values using the session's `agent_cfg.commands` and
    /// stores them in the registry.
    pub fn apply_available_commands(&mut self, commands: &[spur_acp::AvailableCommand]) {
        let handle = self.agent_handle_for_commands();
        let entries: Vec<_> = commands
            .iter()
            .map(|c| crate::agents::build_entry(&handle, &self.agent_cfg.commands, c))
            .collect();
        self.command_registry.set_agent_commands(&handle, entries);
    }

    /// Handle a `SpurEventBody::CommandRegistryDirty` payload. Synthesizes
    /// advertised slash entries from the cached `config_options` and stores
    /// the snapshot so the popup's slash-arg picker can fetch live choices
    /// via `CompletionEnv.session_config_options`.
    pub fn apply_advertised_commands(&mut self, options: &[spur_acp::SessionConfigOption]) {
        let handle = self.agent_handle_for_commands();
        let entries = crate::commands::advertised::AdvertisedSource::entries(&handle, options);
        self.command_registry
            .set_advertised_commands(&handle, entries);
        self.session_config_options = options.to_vec();
    }

    /// Cache the agent capabilities for this session. Captured by the
    /// orchestrator after `session/new` and plumbed through the resume
    /// pipeline; populated `Some(_)` for fresh sessions, left `None` for
    /// sessions resumed before M9 wires `LoadSessionResponse` into
    /// `SpurAgentCaps`. `None` is treated as permissive on read paths.
    pub fn set_spur_agent_caps(
        &mut self,
        caps: Option<std::sync::Arc<spur_acp::SpurAgentCaps>>,
    ) {
        self.spur_agent_caps = caps;
    }

    /// Slash-command popup view: the merged registry filtered by the
    /// cached `SpurAgentCaps`. When caps are absent (resumed sessions
    /// pre-M9), the unfiltered list is returned so pickers stay visible.
    pub fn available_slash_commands(&self) -> Vec<crate::commands::CommandEntry> {
        self.command_registry
            .available_commands_for_session(self.spur_agent_caps.as_deref())
    }

    /// Test-only accessor for the cached snapshot of advertised session
    /// config options.
    #[cfg(any(test, debug_assertions))]
    pub fn session_config_options_for_test(&self) -> &[spur_acp::SessionConfigOption] {
        &self.session_config_options
    }

    /// Test-only accessor: flattened trace text for each entry,
    /// oldest→newest. Used by integration tests in
    /// `tests/session_update_handling.rs`.
    #[doc(hidden)]
    pub fn trace_snapshot_for_test(&self) -> Vec<String> {
        self.react_trace
            .entries_for_test()
            .iter()
            .map(|e| e.text.clone())
            .collect()
    }

    /// Current local time formatted as HH:MM:SS.
    fn now_stamp() -> String {
        crate::components::now_stamp()
    }

    /// Agent kind for this session, used by the adapter layer.
    fn agent_kind(&self) -> spur_acp::AgentKind {
        self.agent_cfg.kind
    }

    /// Set the current session mode and propagate it into the trace pane so
    /// the pane-title badge renders the new mode.
    pub(crate) fn set_current_mode(&mut self, mode: Option<String>) {
        self.current_mode = mode.clone();
        self.react_trace.set_mode(mode);
    }

    pub fn set_edit_mode(&mut self, mode: EditMode) {
        self.input_bar.set_mode(mode);
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.input_bar.insert_paste(text);
        self.dispatch_intent(crate::components::completion_trigger::IntentEvent::Pasted);
    }

    /// Format elapsed time since view was opened.
    fn elapsed(&self) -> String {
        crate::components::format_elapsed(self.started_at)
    }

    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, status: &str) {
        if self.cancelling_in_flight {
            self.input_bar.set_status(
                Some(format!("[{}: cancelling{{spinner}}]", self.agent_name)),
                ActivityKind::Cancelling,
            );
            return;
        }
        let mention_count = self.input_bar.protected_ranges().len();
        let mention_suffix = if mention_count > 0 {
            format!(
                " \u{00b7} {} mention{}",
                mention_count,
                if mention_count > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };

        let (label, activity) = match status {
            "idle" => {
                if mention_count > 0 {
                    (
                        Some(format!(
                            "[{} mention{}]",
                            mention_count,
                            if mention_count > 1 { "s" } else { "" }
                        )),
                        ActivityKind::Idle,
                    )
                } else {
                    (None, ActivityKind::Idle)
                }
            }
            "thinking" => (
                Some(format!(
                    "[{} {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Thinking,
            ),
            "connecting" => (
                Some(format!(
                    "[{}: connecting {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Connecting,
            ),
            "connected" => (
                Some(format!(
                    "[{}: connected{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Idle,
            ),
            "streaming" => (
                Some(format!(
                    "[{} {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Streaming,
            ),
            "ready" => (
                Some(format!("[{}: ready{}]", self.agent_name, mention_suffix)),
                ActivityKind::Idle,
            ),
            "error" => (
                Some(format!("[{}: error{}]", self.agent_name, mention_suffix)),
                ActivityKind::Idle,
            ),
            other if other.starts_with("delegat") => (
                Some(format!(
                    "[{}: {}{}]",
                    self.agent_name, other, mention_suffix
                )),
                ActivityKind::Thinking,
            ),
            other => (
                Some(format!(
                    "[{}: {}{}]",
                    self.agent_name, other, mention_suffix
                )),
                ActivityKind::Idle,
            ),
        };
        self.input_bar.set_status(label, activity);
    }

    pub fn scroll_up(&mut self) {
        self.react_trace.scroll_up();
    }

    pub fn scroll_down(&mut self) {
        self.react_trace.scroll_down();
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        self.react_trace.scroll_up_by(lines);
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        self.react_trace.scroll_down_by(lines);
    }

    /// Add a user message to the ReAct trace for instant feedback.
    pub fn push_user_message(&mut self, text: &str) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::UserMessage,
            text: text.to_string(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    pub fn append_user_message(&mut self, text: &str) {
        self.react_trace
            .append_user_message(text, Self::now_stamp());
    }

    /// Push a system-note trace entry (informational message from the TUI
    /// itself, e.g. stubbed kiro execution).
    pub fn push_system_note(&mut self, msg: impl Into<String>) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Observe { payload: None },
            text: msg.into(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a trace entry showing the current session cost.
    pub fn push_cost_note(&mut self) {
        let msg = format!("Session cost: ${:.2}", self.cost);
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: msg,
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a trace entry telling the user how to persist their current
    /// edit-mode choice. Called whenever the runtime mode diverges from the
    /// configured mode after `Alt-i` or `/vim` — fires per divergent toggle,
    /// not once per session, so repeated cycles will repeat the hint.
    pub fn push_persist_hint(&mut self, mode_label: &str) {
        let msg = format!(
            "{mode_label} mode (session). Persist: spur config set tui.edit_mode {}",
            mode_label.to_ascii_lowercase()
        );
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: msg,
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a system note reflecting the active `cancel_mode`. Called when
    /// the user presses `Esc` to cancel an in-flight stream.
    fn push_cancel_note(&mut self) {
        let text = match self.cancel_mode {
            Some(spur_acp::CancelMode::AcpSoft) => {
                "\u{23f9} Cancellation requested \u{2014} waiting for agent\u{2026}"
            }
            Some(spur_acp::CancelMode::ProcessKill) => {
                "\u{23f9} Stopping agent (process will restart on next message)"
            }
            None => "\u{23f9} Cancellation requested",
        };
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: text.to_string(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Replay conversation history from disk into the trace.
    pub fn replay_history(&mut self, entries: &[spur_acp::HistoryEntry]) {
        // Header to distinguish replayed history from live conversation.
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: "--- Session history (replayed from disk) ---".to_string(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });

        for entry in entries {
            match entry.role.as_str() {
                "user" => {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::UserMessage,
                        text: entry.text.clone(),
                        timestamp: String::new(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
                "assistant" => {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::AgentMessage {
                            agent: self.agent_name.clone(),
                        },
                        text: entry.text.clone(),
                        timestamp: String::new(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
                _ => {}
            }
        }

        // Footer separator before live session.
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: "--- End of history. New messages below ---".to_string(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    pub fn resolve_pending_permissions(&mut self) {
        self.react_trace.resolve_pending_permissions();
    }

    #[cfg(feature = "markdown")]
    pub fn handle_mermaid_completed(
        &mut self,
        ref_id: crate::components::mermaid::MermaidId,
        target_width: u32,
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    ) {
        use crate::components::mermaid::MermaidState;

        // Always release the in-flight slot, success or failure.
        self.in_flight_renders.remove(&ref_id);

        // Retain the source code from the previous state so a future
        // re-raster on bucket-up can re-dispatch without reaching back
        // into MarkdownStream.
        let code = match self.mermaid_registry.get(&ref_id) {
            Some(MermaidState::Pending { code }) => code.clone(),
            Some(MermaidState::Ready { code, .. }) => code.clone(),
            _ => String::new(),
        };

        let state = match result {
            Ok(image) => {
                self.next_image_generation = self.next_image_generation.saturating_add(1);
                MermaidState::Ready {
                    image,
                    code,
                    rastered_at_bucket: target_width,
                    image_generation: self.next_image_generation,
                }
            }
            Err(message) => MermaidState::Error { message },
        };
        self.mermaid_registry.insert(ref_id, state);

        // Mark every markdown stream dirty so the next tick's maybe_flush
        // rebuilds placeholders — transitions Pending→Ready (📊) or →Error (⚠).
        self.react_trace.mark_all_streams_dirty();
    }

    /// Inspect Ready diagrams; emit re-raster requests for any whose
    /// `rastered_at_bucket` is below the current pane's bucket. Coalesced
    /// via `in_flight_renders` so only one request per id can be live.
    /// Two-phase (collect → mutate) for borrow-checker robustness.
    #[cfg(feature = "markdown")]
    pub fn maybe_request_rerasters(&mut self, pane_cols: u16, cell_w_px: u16) {
        use crate::components::mermaid::{raster_width_for_pane, MermaidState};
        let pane_w_px = (pane_cols as u32).saturating_mul(cell_w_px as u32);
        let new_bucket = raster_width_for_pane(pane_w_px);

        let candidates: Vec<(crate::components::mermaid::MermaidId, String)> = self
            .mermaid_registry
            .iter()
            .filter_map(|(id, state)| match state {
                MermaidState::Ready { rastered_at_bucket, code, .. }
                    if *rastered_at_bucket < new_bucket
                        && !self.in_flight_renders.contains(id) =>
                {
                    Some((*id, code.clone()))
                }
                _ => None,
            })
            .collect();

        for (id, code) in candidates {
            self.in_flight_renders.insert(id);
            self.pending_fence_actions.push_back(
                crate::action::Action::MermaidRenderRequest {
                    session: self.session_id.clone(),
                    ref_id: id,
                    code,
                    target_width: new_bucket,
                },
            );
        }
    }

    #[cfg(feature = "markdown")]
    pub fn take_pending_actions(&mut self) -> Vec<crate::action::Action> {
        self.pending_fence_actions.drain(..).collect()
    }

    pub fn push_permission(&mut self, description: &str, countdown: u8) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Permission {
                description: description.to_string(),
                pending: true,
                countdown,
            },
            text: String::new(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    // ── Completion popup wiring ─────────────────────────────────────────

    fn dispatch_intent(&mut self, event: crate::components::completion_trigger::IntentEvent) {
        use crate::components::input_completion::CompletionEnv;
        let env = CompletionEnv {
            command_registry: &self.command_registry,
            mention_registry: &self.mention_registry,
            cwd: &self.cwd,
            scope: crate::mentions::CompletionScope::Session(&self.session_id),
            session_config_options: &self.session_config_options,
        };
        self.completion.dispatch(event, &mut self.input_bar, &env);
    }

    /// Build (error_ids, pending_ids) sets from the mermaid registry for use
    /// in constructing a `StateLookup`.
    #[cfg(feature = "markdown")]
    fn build_state_lookup_sets(
        &self,
    ) -> (
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
    ) {
        use crate::components::mermaid::MermaidState;
        let mut errors = std::collections::HashSet::new();
        let mut pending = std::collections::HashSet::new();
        for (id, state) in &self.mermaid_registry {
            match state {
                MermaidState::Error { .. } => {
                    errors.insert(*id);
                }
                MermaidState::Pending { .. } | MermaidState::Rendering => {
                    pending.insert(*id);
                }
                MermaidState::Ready { .. } => {}
            }
        }
        (errors, pending)
    }
}

impl SessionDetailView {
    fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
        // ── macOS Option-key normalisation ─────────────────────────────
        // macOS terminals send Unicode characters (e.g. `∑` for Option-W)
        // instead of Alt escape sequences when "Use Option as Meta key" is
        // off (the default).  Map the most common US-QWERTY Option-letter
        // characters back to Alt+<ascii> so the keybindings work
        // out-of-the-box.
        let key = super::normalize_macos_option(key);

        // Dismiss the auth banner on any keystroke (before any further routing).
        // The mode-toggle binding below still fires because the action is
        // dispatched regardless.
        if self.auth_error.is_some() {
            self.auth_error = None;
        }

        // Priority 0: Esc-to-cancel takes precedence when a stream is in flight
        // and we're not already cancelling. Second Esc falls through to the
        // existing Esc handlers (popup dismiss / NavigateBack).
        // Exception: in Vim Insert/Visual mode, Esc first exits to Normal mode.
        if matches!(key.code, KeyCode::Esc)
            && self.stream_in_flight
            && !self.cancelling_in_flight
            && !self.input_bar.wants_esc()
        {
            self.cancelling_in_flight = true;
            self.push_cancel_note();
            self.input_bar.set_status(
                Some(format!("[{}: cancelling{{spinner}}]", self.agent_name)),
                ActivityKind::Cancelling,
            );
            return Some(Action::CancelStream {
                session: self.session_id.clone(),
            });
        }

        // Alt-m → cycle session mode between "default" and "plan".
        // Matched early so it works even while the input bar has focus.
        if matches!(key.code, KeyCode::Char('m')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::TogglePlanMode);
        }

        // Alt+s → open session picker. Mirrored by the /sessions slash command.
        // Matched early so it works even while the input bar has focus or a
        // permission prompt is pending (user can bail out of a session at any
        // time; the orchestrator auto-denies the pending permission when the
        // brain is torn down).
        if matches!(key.code, KeyCode::Char('s')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::RequestSessions);
        }

        // Alt+w → jump to Dashboard with Agents panel focused on the
        // highest-priority worker executor.
        if matches!(key.code, KeyCode::Char('w')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::InspectWorkers);
        }

        // Alt+D → toggle inline workers panel collapse.
        if matches!(key.code, KeyCode::Char('d')) && key.modifiers.contains(KeyModifiers::ALT) {
            self.workers_panel_collapsed = !self.workers_panel_collapsed;
            return None;
        }

        // Alt+I → toggle vim/emacs input mode.
        if matches!(key.code, KeyCode::Char('i')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::ToggleVimMode);
        }

        // ── Key ownership is decided from pre-key state ─────────────────
        // This replaces the former post-edit rescue block for j/k/g/G and
        // ensures picker-shell ownership runs before history prev/next.
        enum KeyOwner {
            Composer,
            Picker,
            View,
        }

        let owner = {
            // Pending permission keys outrank even an open picker shell.
            if self.react_trace.has_pending_permission()
                && matches!(key.code, KeyCode::Char('y' | 'n' | 'a'))
            {
                KeyOwner::View
            } else if self.completion.is_active() {
                let is_trigger_driven = self.completion.is_trigger_driven();
                let shell_consumes = if is_trigger_driven {
                    matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
                    ) || ((key.code == KeyCode::Char('c')
                        || key.code == KeyCode::Char('p')
                        || key.code == KeyCode::Char('n'))
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                } else {
                    true
                };
                if shell_consumes {
                    KeyOwner::Picker
                } else {
                    // Trigger-driven shell doesn't consume this editing key;
                    // fall through to Composer so input_bar receives it and
                    // dispatch_intent syncs the shell query.
                    KeyOwner::Composer
                }
            } else {
                // View-level shortcuts are never Composer-owned.
                let is_view_shortcut = (matches!(key.code, KeyCode::Char('o' | 'r'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
                    || (matches!(key.code, KeyCode::Char('v'))
                        && key.modifiers.contains(KeyModifiers::ALT)
                        && self.has_render_picker());
                // Ctrl+P / Ctrl+N drive SessionDetail history when no picker owns them.
                let is_history_nav = matches!(key.code, KeyCode::Char('p' | 'n'))
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if is_view_shortcut || is_history_nav {
                    KeyOwner::View
                } else {
                    // Pending permission keys are never Composer-owned.
                    let is_permission_key = self.react_trace.has_pending_permission()
                        && matches!(key.code, KeyCode::Char('y' | 'n' | 'a'));
                    let is_composer_editing = (matches!(
                        key.code,
                        KeyCode::Char(_)
                            | KeyCode::Backspace
                            | KeyCode::Delete
                            | KeyCode::Left
                            | KeyCode::Right
                            | KeyCode::Home
                            | KeyCode::End
                            | KeyCode::Enter
                            | KeyCode::Up
                            | KeyCode::Down
                    ) || (key.code == KeyCode::Esc
                        && self.input_bar.wants_esc()))
                        && !is_permission_key;

                    if is_composer_editing {
                        // Empty-bar nav chars (j/k/g/G) and Up/Down/Esc are
                        // View-owned scroll/nav keys — no rescue block needed.
                        if self.input_bar.is_empty()
                            && (matches!(key.code, KeyCode::Char('j' | 'k' | 'g' | 'G'))
                                || matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Esc))
                        {
                            KeyOwner::View
                        }
                        // Vim Normal mode-entry keys (i/a/A/I/o/O) fall through
                        // to Composer even when the bar is empty.
                        else if self.input_bar.is_empty()
                            && self.input_bar.is_vim_normal()
                            && matches!(key.code, KeyCode::Char('i' | 'a' | 'A' | 'I' | 'o' | 'O'))
                        {
                            KeyOwner::Composer
                        }
                        // Unrecognized Vim Normal chars are no-ops when empty.
                        else if self.input_bar.is_empty() && self.input_bar.is_vim_normal() {
                            KeyOwner::View
                        } else {
                            KeyOwner::Composer
                        }
                    } else {
                        KeyOwner::View
                    }
                }
            }
        };

        match owner {
            KeyOwner::Picker => {
                let _ = self.completion.handle_picker_key(key, &mut self.input_bar);
                None
            }

            KeyOwner::Composer => {
                use crate::components::completion_trigger::IntentEvent;
                use crate::components::input_bar::HandleOutcome;
                match self.input_bar.handle_key(key) {
                    HandleOutcome::Submit(_, _) => {
                        if let Some(ref mut banner) = self.resume_banner {
                            banner.record_message_sent();
                        }
                        self.dispatch_intent(IntentEvent::Submitted);
                        if let Some((text, ranges, interrupt)) =
                            self.input_bar.take_submit_capture()
                        {
                            use crate::commands::submit_router::{route_with_caps, SubmitDecision};
                            let dec = route_with_caps(
                                &text,
                                &ranges,
                                &self.command_registry,
                                interrupt,
                                self.spur_agent_caps.as_deref(),
                            );
                            return match dec {
                                SubmitDecision::Empty => None,
                                SubmitDecision::Send {
                                    mut blocks,
                                    interrupt,
                                } => {
                                    if self.role == "brain" {
                                        let _ = crate::mentions::hint::prepend_worker_hint(
                                            &mut blocks,
                                            &ranges,
                                            &self.known_worker_names,
                                        );
                                    }
                                    if self.is_cleared() {
                                        Some(Action::NewSessionWithMessage { blocks, interrupt })
                                    } else {
                                        Some(Action::SendMessage {
                                            session: self.session_id.clone(),
                                            blocks,
                                            interrupt,
                                        })
                                    }
                                }
                                SubmitDecision::Local { action } => Some(action),
                                SubmitDecision::VendorExec { method, params } => {
                                    if self.is_cleared() {
                                        None
                                    } else {
                                        Some(Action::VendorExec {
                                            session: self.session_id.clone(),
                                            method,
                                            params,
                                        })
                                    }
                                }
                                SubmitDecision::SetSessionConfigOption { config_id, value } => {
                                    if self.is_cleared() {
                                        None
                                    } else {
                                        Some(Action::SetSessionConfigOption { config_id, value })
                                    }
                                }
                                SubmitDecision::SetSessionModel { value } => {
                                    if self.is_cleared() {
                                        None
                                    } else {
                                        Some(Action::SetSessionModel {
                                            session_id: self.session_id.clone(),
                                            value,
                                        })
                                    }
                                }
                            };
                        }
                        None
                    }
                    HandleOutcome::Key(intent) => {
                        self.dispatch_intent(intent);
                        None
                    }
                }
            }

            KeyOwner::View => {
                // Ctrl+O → toggle collapse/expand on Observe entries.
                if matches!(key.code, KeyCode::Char('o'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.react_trace.toggle_observe_collapsed();
                    return None;
                }

                // Ctrl+P / Ctrl+N → input history navigation.
                if matches!(key.code, KeyCode::Char('p'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.input_bar.history_prev();
                    self.dispatch_intent(
                        crate::components::completion_trigger::IntentEvent::SetText,
                    );
                    return None;
                }
                if matches!(key.code, KeyCode::Char('n'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.input_bar.history_next();
                    self.dispatch_intent(
                        crate::components::completion_trigger::IntentEvent::SetText,
                    );
                    return None;
                }

                #[cfg(feature = "markdown")]
                if matches!(key.code, KeyCode::Char('v'))
                    && key.modifiers.contains(KeyModifiers::ALT)
                    && self.render_picker.is_some()
                {
                    return Some(Action::NavigateTo(ViewId::MermaidOverlay(
                        self.session_id.clone(),
                    )));
                }

                // Permission handling when a permission is pending.
                if self.react_trace.has_pending_permission() {
                    use crate::action::PermissionChoice;
                    match key.code {
                        KeyCode::Char('y') => {
                            return Some(Action::PermissionGrant(PermissionChoice::Allow));
                        }
                        KeyCode::Char('n') => {
                            return Some(Action::PermissionGrant(PermissionChoice::Deny));
                        }
                        KeyCode::Char('a') => {
                            return Some(Action::PermissionGrant(PermissionChoice::AlwaysAllow));
                        }
                        _ => {}
                    }
                }

                // Ctrl+R / Alt+R → open history PickerShell.
                if matches!(key.code, KeyCode::Char('r'))
                    && (key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::ALT))
                    && !self.completion.is_active()
                {
                    let history = self.input_bar.history().to_vec();
                    self.completion.open_history(history);
                    return None;
                }

                // PageUp/PageDown work regardless of input bar state.
                match key.code {
                    KeyCode::PageUp => {
                        self.react_trace.page_up();
                        return Some(Action::ScrollUp);
                    }
                    KeyCode::PageDown => {
                        self.react_trace.page_down();
                        return Some(Action::ScrollDown);
                    }
                    _ => {}
                }

                // Empty-bar nav: j/k/Up/Down scroll, g/G jump, Esc back.
                if self.input_bar.is_empty() {
                    match key.code {
                        KeyCode::Char('j') => {
                            self.react_trace.scroll_down();
                            return Some(Action::ScrollDown);
                        }
                        KeyCode::Char('k') => {
                            self.react_trace.scroll_up();
                            return Some(Action::ScrollUp);
                        }
                        KeyCode::Char('g') => {
                            self.react_trace.scroll_to_top();
                            return Some(Action::ScrollToTop);
                        }
                        KeyCode::Char('G') => {
                            self.react_trace.scroll_to_bottom();
                            return Some(Action::ScrollToBottom);
                        }
                        KeyCode::Up => {
                            self.react_trace.scroll_up();
                            return Some(Action::ScrollUp);
                        }
                        KeyCode::Down => {
                            self.react_trace.scroll_down();
                            return Some(Action::ScrollDown);
                        }
                        KeyCode::Esc => {
                            return Some(Action::NavigateBack);
                        }
                        _ => {}
                    }
                }

                None
            }
        }
    }
}

impl View for SessionDetailView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        // Resume banner key consumption — must happen BEFORE normal key routing.
        if let Some(ref mut banner) = self.resume_banner {
            if banner.is_consuming_keys() {
                if let Some(action) = banner.handle_key(key) {
                    return Some(action);
                }
                // If banner handled the key but returned None (e.g. Esc fading),
                // still allow the key to fall through UNLESS it was Esc.
                if key.code == KeyCode::Esc {
                    return None;
                }
            }
        }
        let key = super::normalize_macos_option(key);
        if matches!(key.code, KeyCode::Char('p')) && key.modifiers.contains(KeyModifiers::ALT) {
            if ctx
                .plan_projection
                .current_for_session(self.session_id())
                .is_some()
            {
                return Some(Action::NavigateTo(ViewId::PlanInspector(
                    self.session_id.clone(),
                )));
            }
            self.input_bar.set_status(
                Some("No tracked plan for this session yet".into()),
                ActivityKind::Idle,
            );
            return None;
        }
        let action = self.handle_key_inner(key);
        // Arm the draft-save debounce whenever the InputBar text diverges
        // from the last persisted value. This covers inserts, deletes, and
        // the empty-after-send case (where sending clears the bar to "" —
        // if the previously-persisted draft was non-empty, we want to
        // overwrite it with the now-empty value).
        let current_text = self.input_bar.text();
        if current_text != self.last_persisted_draft {
            self.last_draft_change_at = Some(std::time::Instant::now());
        }
        action
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, ctx: &super::ViewContext) {
        // Update LoadState from milestone events (Tranche 2 Task 5).
        self.apply_milestone_event(event);

        match &event.body {
            SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                // Read-only mirror of session-scoped state (mode, usage,
                // available commands). Handled before the trace-rendering
                // match so we always capture it regardless of whether a
                // display arm fires below.
                crate::app::apply_session_update(self, &notification.update);

                // Flag streaming state for observable turn progress. This is
                // the caller's responsibility — the shared dispatcher is
                // agnostic to session lifecycle state. Tool-bearing progress
                // and plan updates arm the stream alongside text/thought
                // chunks: a valid ACP turn can begin with ToolCall /
                // ToolCallUpdate / Plan before any text chunk, and Esc-cancel
                // plus the in-flight hint both key off stream_in_flight.
                // UsageUpdate / CurrentModeUpdate are mirrored session state
                // and do not by themselves prove visible turn progress.
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(_)
                    | spur_acp::SessionUpdate::AgentMessageChunk(_)
                    | spur_acp::SessionUpdate::ToolCall(_)
                    | spur_acp::SessionUpdate::ToolCallUpdate(_)
                    | spur_acp::SessionUpdate::Plan(_) => {
                        self.stream_in_flight = true;
                    }
                    _ => {}
                }

                let agent_name = self.agent_name.clone();
                let agent_kind = self.agent_kind();
                let skip_plan_trace = ctx
                    .plan_projection
                    .current_for_session(self.session_id())
                    .is_some();
                let mut ctx = crate::components::react_trace::dispatch::DispatchCtx {
                    agent_name: agent_name.as_str(),
                    agent_kind,
                    now_stamp: Self::now_stamp,
                    tool_depth: &mut self.tool_depth,
                    skip_plan_trace,
                };
                crate::components::react_trace::dispatch::dispatch_session_update(
                    &mut self.react_trace,
                    &notification.update,
                    &mut ctx,
                );
            }

            SpurEventBody::DelegationRequested {
                from,
                to_agent,
                task,
                request_id,
                delegation_plan: _,
                issue_id: _,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                self.set_brain_status(&format!("delegating to {}", to_agent));
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Delegate {
                        agent: to_agent.clone(),
                        task: task.clone(),
                        status: "delegated".to_string(),
                        request_id: Some(request_id.clone()),
                        executor_id: None,
                    },
                    text: String::new(),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }

            SpurEventBody::DelegationDispatched {
                from,
                request_id,
                executor_id,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                // Find the most recent Delegate entry with matching request_id
                // and attach the executor_id.
                self.react_trace.attach_executor_id(request_id, executor_id);
            }

            SpurEventBody::DelegationCompleted {
                worker_session,
                status,
            } => {
                // Update the matching Delegate trace entry so its status
                // renders correctly even when lineage isn't yet available.
                // worker_session.0 carries the request_id / executor_id.
                let status_label = match status {
                    spur_acp::DelegationStatus::Success => "done",
                    spur_acp::DelegationStatus::Failed { .. } => "failed",
                    spur_acp::DelegationStatus::Conflict { .. } => "conflict",
                    spur_acp::DelegationStatus::Timeout => "timed out",
                    spur_acp::DelegationStatus::Rejected { .. } => "rejected",
                    spur_acp::DelegationStatus::Modified { .. } => "modified",
                    spur_acp::DelegationStatus::TimedOut { .. } => "timed out",
                    spur_acp::DelegationStatus::Cancelled { .. } => "cancelled",
                    _ => "completed",
                };
                self.react_trace
                    .update_delegate_status(&worker_session.0, status_label);
            }

            SpurEventBody::PromptDispatched {
                session,
                turn_kind,
                continuations_count,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                // Friendly trace note when the brain is re-entered with
                // worker continuations (autonomous or merged turn).
                let note = match turn_kind.as_str() {
                    "continuation_only" => {
                        if *continuations_count == 1 {
                            "▸ Brain resuming with 1 worker result".to_string()
                        } else {
                            format!(
                                "▸ Brain resuming with {} worker results",
                                continuations_count
                            )
                        }
                    }
                    "merged" => {
                        if *continuations_count == 1 {
                            "▸ Merging user message with 1 worker result".to_string()
                        } else {
                            format!(
                                "▸ Merging user message with {} worker results",
                                continuations_count
                            )
                        }
                    }
                    _ => return, // user_only — no note needed
                };
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Think,
                    text: note,
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }

            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason,
                ..
            } => {
                // This is a system-level event without session scoping;
                // show it for the active session so the user knows a
                // promised continuation was lost.
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: format!("⚠ Continuation dropped for {}: {:?}", delegation_id, reason),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }

            SpurEventBody::CostUpdate {
                session,
                estimated_cost_usd,
                ..
            } => {
                if session.0 == self.session_id.0 {
                    self.cost += estimated_cost_usd;
                }
            }

            SpurEventBody::TurnComplete { session } => {
                if session.0 == self.session_id.0 {
                    self.stream_in_flight = false;
                    self.cancelling_in_flight = false;
                    self.tool_depth.clear();
                    #[cfg(feature = "markdown")]
                    {
                        use crate::components::markdown_stream::StateLookup;

                        let (error_ids, pending_ids) = self.build_state_lookup_sets();
                        let states = StateLookup {
                            errors: &error_ids,
                            pending: &pending_ids,
                        };
                        for (_entry_idx, fence) in self.react_trace.force_flush_all(&states) {
                            self.mermaid_registry.insert(
                                fence.id,
                                crate::components::mermaid::MermaidState::Pending {
                                    code: fence.code.clone(),
                                },
                            );
                            self.in_flight_renders.insert(fence.id);
                            self.pending_fence_actions.push_back(
                                crate::action::Action::MermaidRenderRequest {
                                    session: self.session_id.clone(),
                                    ref_id: fence.id,
                                    code: fence.code,
                                    target_width: {
                                        let cell_w_px = self
                                            .render_picker
                                            .as_ref()
                                            .map(|p| p.font_size().0)
                                            .unwrap_or(8);
                                        // Note: pane width at fence-emit time is not directly
                                        // available; use the last known render width if cached,
                                        // else smallest bucket. The next render frame's
                                        // maybe_request_rerasters will upgrade if needed.
                                        let pane_w_cols = self
                                            .react_trace
                                            .last_render_width()
                                            .unwrap_or(80);
                                        crate::components::mermaid::raster_width_for_pane(
                                            (pane_w_cols as u32).saturating_mul(cell_w_px as u32),
                                        )
                                    },
                                },
                            );
                        }
                    }
                }
            }

            SpurEventBody::BrainError { session, message } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Observe { payload: None },
                        text: format!("BRAIN ERROR: {}", message),
                        timestamp: Self::now_stamp(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
            }
            SpurEventBody::BrainReconnecting {
                session,
                brain_name,
                reason,
            } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Observe { payload: None },
                        text: format!("brain '{}' reconnecting… ({})", brain_name, reason),
                        timestamp: Self::now_stamp(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
            }
            SpurEventBody::BrainReconnected {
                session,
                brain_name,
                outcome,
            } => {
                if session.0 == self.session_id.0 {
                    let text = match outcome {
                        spur_acp::LoadOutcome::Restored => {
                            format!(
                                "brain '{}' reconnected — state restored. Your last prompt/command was dropped; retype to retry.",
                                brain_name
                            )
                        }
                        spur_acp::LoadOutcome::FellBackToNew { reason } => {
                            format!(
                                "brain '{}' reconnected — started FRESH ({}); prior context wiped. Retype to continue.",
                                brain_name, reason
                            )
                        }
                    };
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Observe { payload: None },
                        text,
                        timestamp: Self::now_stamp(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
            }
            SpurEventBody::BrainReconnectFailed {
                session,
                brain_name,
                reason,
            } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Observe { payload: None },
                        text: format!("brain '{}' reconnect FAILED: {}", brain_name, reason),
                        timestamp: Self::now_stamp(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
            }

            SpurEventBody::AgentExtNotification {
                session,
                method,
                params,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                let cfg = self.agent_cfg.clone();

                // Ingest bindings: decode params → delegate to apply_available_commands.
                for binding in &cfg.commands.ingest {
                    if &binding.method != method {
                        continue;
                    }
                    if let Some(parsed) = crate::agents::run_ingest_hook(binding, params) {
                        self.apply_available_commands(&parsed);
                    }
                }

                // Response bindings: render the payload according to `render` kind.
                for binding in &cfg.commands.response {
                    if &binding.method != method {
                        continue;
                    }
                    match binding.render {
                        spur_acp::ResponseRenderKind::SystemNote => {
                            let handle = self.agent_handle_for_commands();
                            self.push_system_note(format!(
                                "\u{27e8}{handle}\u{27e9} response: {}",
                                params
                            ));
                        }
                    }
                }
            }

            SpurEventBody::AgentSessionReady {
                session,
                resumed,
                cancel_mode,
                fs_unsafe,
                ..
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                self.cancel_mode = Some(*cancel_mode);
                self.fs_unsafe = *fs_unsafe;
                if *resumed {
                    self.push_system_note("Resumed from prior conversation".to_string());
                }
            }

            SpurEventBody::CommandRegistryDirty {
                session,
                config_options,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                self.apply_advertised_commands(config_options);
            }

            // All other event types are not relevant to this session view.
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.react_trace.tick();
        self.input_bar.tick();
        if let Some(ref mut banner) = self.resume_banner {
            banner.tick();
        }
        #[cfg(feature = "markdown")]
        {
            use crate::components::markdown_stream::StateLookup;

            let (error_ids, pending_ids) = self.build_state_lookup_sets();
            let states = StateLookup {
                errors: &error_ids,
                pending: &pending_ids,
            };

            for (_entry_idx, fence) in self.react_trace.drain_fence_dispatches(&states) {
                self.mermaid_registry.insert(
                    fence.id,
                    crate::components::mermaid::MermaidState::Pending {
                        code: fence.code.clone(),
                    },
                );
                self.in_flight_renders.insert(fence.id);
                self.pending_fence_actions
                    .push_back(crate::action::Action::MermaidRenderRequest {
                        session: self.session_id.clone(),
                        ref_id: fence.id,
                        code: fence.code,
                        target_width: {
                            let cell_w_px = self
                                .render_picker
                                .as_ref()
                                .map(|p| p.font_size().0)
                                .unwrap_or(8);
                            // Note: pane width at fence-emit time is not directly
                            // available; use the last known render width if cached,
                            // else smallest bucket. The next render frame's
                            // maybe_request_rerasters will upgrade if needed.
                            let pane_w_cols = self
                                .react_trace
                                .last_render_width()
                                .unwrap_or(80);
                            crate::components::mermaid::raster_width_for_pane(
                                (pane_w_cols as u32).saturating_mul(cell_w_px as u32),
                            )
                        },
                    });
            }
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        self.render_inner(
            frame,
            area,
            Some(ctx.lineage),
            ctx.plan_projection.current_for_session(self.session_id()),
            ctx.license_badge,
            ctx.flag_summary,
        );
    }
}

fn build_auth_banner_widget<'a>(message: &'a str) -> Paragraph<'a> {
    Paragraph::new(message)
        .style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title("Authentication required"),
        )
}

fn build_session_error_widget<'a>(message: &'a str) -> Paragraph<'a> {
    Paragraph::new(message)
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title("Session error"),
        )
}

impl SessionDetailView {
    fn render_inner(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
        tracked_plan: Option<&spur_core::TrackedPlan>,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        flag_summary: Option<(usize, usize)>,
    ) {
        // Pre-ready render path: show a status label until LoadState::Ready.
        match &self.load_state {
            LoadState::Retiring => {
                render_load_label(frame, area, "Retiring previous session…");
                return;
            }
            LoadState::Connecting { brain_name } => {
                let label = if brain_name.is_empty() {
                    "Connecting to brain…".to_string()
                } else {
                    format!("Connecting to {brain_name}…")
                };
                render_load_label(frame, area, &label);
                return;
            }
            LoadState::Loading => {
                render_load_label(frame, area, "Loading session history…");
                return;
            }
            LoadState::Failed { message } => {
                render_error_label(frame, area, message);
                return;
            }
            LoadState::Ready => {
                // Fall through to the full render path below.
            }
        }

        let elapsed = self.elapsed();

        // Reserve the top row for the (non-blocking) resume banner when
        // visible. Subsequent banner/content layout operates on `area_rest`.
        let (resume_banner_area, area_rest) = if self.banner_is_visible() {
            let banner = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 1,
            };
            let rest = Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width,
                height: area.height.saturating_sub(1),
            };
            (Some(banner), rest)
        } else {
            (None, area)
        };

        // If an auth error is active, split off the top 3 rows for a red
        // banner. This preserves the rest of the layout exactly as before.
        let (banner_area, content_area) = if self.auth_error.is_some() {
            let [banner, content] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area_rest);
            (Some(banner), content)
        } else {
            (None, area_rest)
        };

        if let (Some(banner_area), Some(msg)) = (banner_area, self.auth_error.as_ref()) {
            let banner = build_auth_banner_widget(msg.as_str());
            frame.render_widget(banner, banner_area);
        }

        let input_height = self.input_bar.required_height(content_area.width);
        let unsafe_banner_height = u16::from(self.fs_unsafe);

        // Compute workers panel height (dynamic: 0 when no active workers).
        // Suppress on very small terminals to avoid squeezing the trace.
        let executor_ids = self.react_trace.active_executor_ids();
        let workers_h = if content_area.height < 12 {
            0
        } else {
            lineage
                .map(|lin| {
                    crate::components::workers_panel::compute_height(
                        lin,
                        &executor_ids,
                        self.workers_panel_collapsed,
                    )
                })
                .unwrap_or(0)
        };

        let chunks = Layout::vertical([
            Constraint::Length(1),                    // header
            Constraint::Min(4),                       // react trace (fills)
            Constraint::Length(workers_h),            // workers panel
            Constraint::Length(unsafe_banner_height), // unsafe-fs banner
            Constraint::Length(input_height),         // input bar
            Constraint::Length(1),                    // status bar
        ])
        .split(content_area);

        // ── Header: breadcrumb + elapsed + cost ─────────────────────────
        let [header_left, header_right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(48)]).areas(chunks[0]);
        let header = Line::from(vec![
            Span::styled(" Dashboard > ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &self.agent_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", self.role),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(&elapsed, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                format!("${:.2}", self.cost),
                Style::default().fg(Color::Yellow),
            ),
            if self.fs_unsafe {
                Span::styled(
                    "  unsafe-fs",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("")
            },
        ]);
        frame.render_widget(Paragraph::new(header), header_left);
        if let Some(plan) = tracked_plan {
            crate::components::plan_pulse::render(frame, header_right, plan);
        }

        // ── React trace ─────────────────────────────────────────────────
        #[cfg(feature = "markdown")]
        {
            let mut ctx = crate::components::react_trace::RenderContext {
                mermaid_registry: &self.mermaid_registry,
                picker: self.render_picker.as_ref(),
                image_cache: &mut self.image_cache,
            };
            self.react_trace
                .render_with_ctx(frame, chunks[1], &mut ctx, lineage);
        }
        #[cfg(not(feature = "markdown"))]
        self.react_trace.render(frame, chunks[1], lineage);

        // After react_trace render — re-raster on bucket-up.
        #[cfg(feature = "markdown")]
        {
            let cell_w_px = self
                .render_picker
                .as_ref()
                .map(|p| p.font_size().0)
                .unwrap_or(8);
            self.maybe_request_rerasters(chunks[1].width, cell_w_px);
        }

        // ── Workers panel ───────────────────────────────────────────────
        if let Some(lin) = lineage {
            if workers_h > 0 {
                crate::components::workers_panel::render(
                    frame,
                    chunks[2],
                    lin,
                    &executor_ids,
                    self.workers_panel_collapsed,
                );
            }
        }

        // ── Input bar ───────────────────────────────────────────────────
        if self.fs_unsafe {
            let banner = Line::from(Span::styled(
                " unsafe-fs: flock unsupported on this volume - multi-window protection OFF ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ));
            frame.render_widget(Paragraph::new(banner), chunks[3]);
        }

        // Render in "inert" style (dimmed border, no terminal cursor) when
        // a PickerShell has the focus — the shell owns the cursor.
        if self.completion.is_active() {
            self.input_bar.render_inert(frame, chunks[4]);
        } else {
            self.input_bar.render(frame, chunks[4]);
        }

        // ── PickerShell overlay ─────────────────────────────────────────
        self.completion.render(frame, chunks[4], area);

        // ── Status bar (with live worker counts) ────────────────────────
        let (running, pending_review) = lineage
            .map(|lin| {
                let mut r = 0usize;
                let mut p = 0usize;
                for eid in &executor_ids {
                    if let Some(node) = lin.node(&spur_core::ExecutorId(eid.clone())) {
                        match node.phase {
                            spur_acp::domain::events::LifecycleState::Running
                            | spur_acp::domain::events::LifecycleState::Spawning
                            | spur_acp::domain::events::LifecycleState::Resuming => r += 1,
                            spur_acp::domain::events::LifecycleState::AwaitingReview => p += 1,
                            _ => {}
                        }
                    }
                }
                (r, p)
            })
            .unwrap_or((0, 0));
        let caps = self.spur_agent_caps.as_deref();
        // Model freshness still derives from the frozen caps snapshot; M10.2
        // will add live model writeback when that state has a mutable owner.
        let model_label = caps.and_then(spur_acp::SpurAgentCaps::current_model_label);
        let effort_label =
            spur_acp::SpurAgentCaps::effort_label_from(&self.session_config_options);
        let usage_supported = caps
            .map(spur_acp::SpurAgentCaps::usage_supported)
            .unwrap_or(true);

        StatusBar::render(
            frame,
            chunks[5],
            StatusBarProps {
                view: &ViewId::SessionDetail(self.session_id.clone()),
                running,
                pending_review,
                total_cost: self.cost,
                elapsed: &elapsed,
                current_mode: self.current_mode.as_deref(),
                current_model_label: model_label.as_deref(),
                current_effort_label: effort_label.as_deref(),
                usage_supported,
                context_used: self.context_used,
                context_size: self.context_size,
                stream_in_flight: self.stream_in_flight && !self.cancelling_in_flight,
                esc_consumed_by_composer: self.input_bar.wants_esc(),
                issue_count: 0,
                alert_summary: None,
                license_badge,
                flag_summary,
                view_hint_override: None,
            },
        );

        // ── Resume banner (top row, if visible) ─────────────────────────
        if let (Some(banner), Some(rect)) = (self.resume_banner.as_ref(), resume_banner_area) {
            banner.render(frame, rect);
            if self.ready_banner.is_some() {
                tracing::warn!(
                    "ready_banner and resume_banner both set — auto-resume wins (spec R3 violation)"
                );
            }
        } else if let (Some(ready_text), Some(rect)) =
            (self.ready_banner.as_ref(), resume_banner_area)
        {
            let styled = Paragraph::new(ready_text.as_str())
                .style(Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC));
            frame.render_widget(styled, rect);
        }
    }

    /// Test-only: read current InputBar text.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_text_for_test(&self) -> String {
        self.input_bar.text()
    }

    /// Test-only: mutable InputBar access for seeding history in tests.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_mut_for_test(&mut self) -> &mut crate::components::input_bar::InputBar {
        &mut self.input_bar
    }

    /// Test-only: read tool_depth map.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn tool_depth_for_test(&self) -> &std::collections::HashMap<String, u8> {
        &self.tool_depth
    }

    /// Test-only: mutable tool_depth map for seeding tests.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn tool_depth_for_test_mut(&mut self) -> &mut std::collections::HashMap<String, u8> {
        &mut self.tool_depth
    }
}

// ─── LoadState render helpers ───────────────────────────────────────────────

/// Render a centered single-line status label for pre-ready LoadStates
/// (`Retiring`, `Connecting`, `Loading`).
fn render_load_label(frame: &mut Frame, area: Rect, label: &str) {
    use ratatui::layout::Alignment;
    use ratatui::widgets::{Block, Borders};
    let para = Paragraph::new(label)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));
    // Centre vertically by splitting the area in thirds.
    let [_, mid, _] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Percentage(40),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Percentage(60),
    ])
    .areas(area);
    frame.render_widget(para, mid);
}

/// Render a red error panel for `LoadState::Failed`.
fn render_error_label(frame: &mut Frame, area: Rect, message: &str) {
    let para = build_session_error_widget(message);
    let [_, mid, _] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Percentage(40),
        ratatui::layout::Constraint::Min(3),
        ratatui::layout::Constraint::Percentage(60),
    ])
    .areas(area);
    frame.render_widget(para, mid);
}

// ─── Formatting helpers (test-only; production path uses dispatch.rs) ───

#[cfg(test)]
/// Extract renderable text from a `ToolCallContent` slice.
///
/// Handles all known variants:
/// - `Content` — returns the inner text (non-text blocks silently skipped).
/// - `Diff`     — formats as a truncated unified-style diff (max `DIFF_MAX_LINES` body lines).
/// - `Terminal` — returns a placeholder `[terminal: <id>]`.
/// - Unknown future variants — silently ignored (`ToolCallContent` is `#[non_exhaustive]`).
///
/// Returns `None` if nothing renderable was produced.
fn extract_tool_call_text(content: &[spur_acp::ToolCallContent]) -> Option<String> {
    use spur_acp::ToolCallContent;
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content(cb) => {
                if let spur_acp::ContentBlock::Text(tc) = &cb.content {
                    out.push_str(&tc.text);
                }
                // Non-Text ContentBlock variants (Image, Audio, Resource) silently skipped.
            }
            ToolCallContent::Diff(diff) => {
                out.push_str(&format_diff_truncated(
                    &diff.path.display().to_string(),
                    diff.old_text.as_deref(),
                    &diff.new_text,
                ));
            }
            ToolCallContent::Terminal(term) => {
                // TerminalId derives Display; fall back to .0 (Arc<str>) if needed.
                out.push_str(&format!("[terminal: {}]", term.terminal_id));
            }
            _ => {
                // ToolCallContent is #[non_exhaustive]; ignore unknown variants.
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
const DIFF_MAX_LINES: usize = 40;

/// Format a diff as a simplified unified-diff string, capped at `DIFF_MAX_LINES` body lines.
///
/// Old lines are prefixed with `-`, new lines with `+`. This is NOT an LCS diff;
/// it renders the old text as all-deletions and the new text as all-additions,
/// matching how `ObservePayload::EditResult.diff` is rendered elsewhere in the TUI.
#[cfg(test)]
fn format_diff_truncated(path: &str, old: Option<&str>, new_: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path));
    out.push_str(&format!("+++ b/{}\n", path));

    let mut body_lines: usize = 0;
    let mut truncated_count: usize = 0;

    if let Some(old_text) = old {
        for line in old_text.lines() {
            if body_lines >= DIFF_MAX_LINES {
                truncated_count += 1;
                continue;
            }
            out.push_str(&format!("-{}\n", line));
            body_lines += 1;
        }
    }
    for line in new_.lines() {
        if body_lines >= DIFF_MAX_LINES {
            truncated_count += 1;
            continue;
        }
        out.push_str(&format!("+{}\n", line));
        body_lines += 1;
    }
    if truncated_count > 0 {
        out.push_str(&format!("... ({} more lines)\n", truncated_count));
    }
    out
}

#[cfg(test)]
mod banner_tests {
    use super::*;
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};

    fn render_auth_banner(message: &str, area: Rect) -> Buffer {
        let banner = super::build_auth_banner_widget(message);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(banner, area)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_session_error(message: &str, area: Rect) -> Buffer {
        let banner = super::build_session_error_widget(message);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(banner, area)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn rendered_text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|cell| cell.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn auth_banner_renders_title_body_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 64, 3);
        let message = "Run `spur login` to continue";
        let buf = render_auth_banner(message, area);
        let rendered = rendered_text(&buf);

        assert!(
            rendered.contains("Authentication required"),
            "auth banner title must appear. Rendered:\n{rendered}"
        );
        assert!(
            rendered.contains(message),
            "auth banner body must appear. Rendered:\n{rendered}"
        );

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).expect("cell should exist in banner area");
                assert_eq!(
                    cell.bg,
                    Color::Red,
                    "cell ({x}, {y}) should have red background"
                );
                assert_eq!(
                    cell.fg,
                    Color::White,
                    "cell ({x}, {y}) should have white foreground"
                );
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "cell ({x}, {y}) should be bold"
                );
                assert_ne!(
                    cell.symbol(),
                    "│",
                    "cell ({x}, {y}) should not render a vertical border glyph"
                );
            }
        }
    }

    #[test]
    fn session_error_renders_title_body_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 64, 3);
        let message = "executor exited before ready";
        let buf = render_session_error(message, area);
        let rendered = rendered_text(&buf);

        assert!(
            rendered.contains("Session error"),
            "session error title must appear. Rendered:\n{rendered}"
        );
        assert!(
            rendered.contains(message),
            "session error body must appear. Rendered:\n{rendered}"
        );
        assert!(
            !rendered.contains('│'),
            "session error must not render vertical border glyphs. Rendered:\n{rendered}"
        );

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf
                    .cell((x, y))
                    .expect("cell should exist in session error area");
                assert_eq!(
                    cell.bg,
                    Color::Red,
                    "cell ({x}, {y}) should have red background"
                );
                assert_eq!(
                    cell.fg,
                    Color::White,
                    "cell ({x}, {y}) should have white foreground"
                );
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "cell ({x}, {y}) should be bold"
                );
                assert_ne!(
                    cell.symbol(),
                    "│",
                    "cell ({x}, {y}) should not render a vertical border glyph"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "markdown"))]
mod maybe_request_rerasters_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState, RASTER_BUCKETS};
    use std::sync::Arc;
    use image::{DynamicImage, RgbaImage};

    #[allow(dead_code)]
    fn buckets_constant_check() {
        // Touch RASTER_BUCKETS so the import isn't dead in builds where
        // tests skip every assertion that references it.
        let _ = RASTER_BUCKETS;
    }

    fn ready_at(bucket: u32, gen: u64) -> MermaidState {
        MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "graph TD\nA-->B".into(),
            rastered_at_bucket: bucket,
            image_generation: gen,
        }
    }

    #[test]
    fn maybe_request_rerasters_skips_when_bucket_unchanged() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 80 cols × 8 px = 640 → bucket 800. No upgrade.
        view.maybe_request_rerasters(80, 8);
        assert!(view.pending_fence_actions.is_empty(),
            "no requests when bucket unchanged");
    }

    #[test]
    fn maybe_request_rerasters_emits_for_lower_bucketed_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Upgrade.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(1)));
    }

    #[test]
    fn maybe_request_rerasters_skips_pending() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(2),
            MermaidState::Pending { code: "g".into() },
        );
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn maybe_request_rerasters_skips_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(3), ready_at(800, 1));
        view.in_flight_renders.insert(MermaidId(3));
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty(),
            "no duplicate requests for in-flight ids");
    }

    #[test]
    fn maybe_request_rerasters_skips_just_landed_at_new_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(4), ready_at(1600, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Already there.
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn rerasters_coalesce_during_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(5), ready_at(800, 1));

        // First trigger: pane grows to bucket 1200.
        view.maybe_request_rerasters(150, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);

        // Second trigger BEFORE completion: pane grows to bucket 2000.
        view.maybe_request_rerasters(250, 8);
        // Still only one — id is in_flight, gated.
        assert_eq!(view.pending_fence_actions.len(), 1);
    }

    #[test]
    fn handle_completed_clears_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.in_flight_renders.insert(MermaidId(6));
        view.mermaid_registry.insert(
            MermaidId(6),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(6), 800, Ok(img));
        assert!(!view.in_flight_renders.contains(&MermaidId(6)));
    }

    #[test]
    fn handle_completed_records_target_width_on_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(7),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(7), 1600, Ok(img));
        match view.mermaid_registry.get(&MermaidId(7)) {
            Some(MermaidState::Ready { rastered_at_bucket, .. }) => {
                assert_eq!(*rastered_at_bucket, 1600);
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_ready_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(8), MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "ORIGINAL".into(),
            rastered_at_bucket: 800,
            image_generation: 1,
        });
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(8), 1600, Ok(img));
        match view.mermaid_registry.get(&MermaidId(8)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "ORIGINAL"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_pending_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(9),
            MermaidState::Pending { code: "PENDING_SOURCE".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(9), 800, Ok(img));
        match view.mermaid_registry.get(&MermaidId(9)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "PENDING_SOURCE"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_bumps_image_generation_on_ok() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(10),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(10), 800, Ok(img.clone()));
        let gen1 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready { image_generation, .. }) => *image_generation,
            _ => panic!(),
        };
        view.handle_mermaid_completed(MermaidId(10), 1200, Ok(img));
        let gen2 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready { image_generation, .. }) => *image_generation,
            _ => panic!(),
        };
        assert!(gen2 > gen1, "generation must monotonically increase");
    }

    #[test]
    fn handle_completed_never_decreases_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(11), MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "g".into(),
            rastered_at_bucket: 1600,
            image_generation: 1,
        });
        // Even if a stale completion arrives with a smaller bucket, the
        // handler stores the COMPLETION's bucket — but maybe_request_rerasters
        // never EMITS at a smaller bucket (test is for the trigger, not the
        // handler). The handler simply records what arrived.
        // I-R1 is enforced at the EMIT side (maybe_request_rerasters compares
        // current_bucket against rastered_at_bucket and only emits if greater).
        // This test verifies the emit side.
        view.maybe_request_rerasters(80, 8); // pane_w_px=640 → bucket 800
        assert!(
            view.pending_fence_actions.is_empty(),
            "must never emit when current_bucket < rastered_at_bucket"
        );
    }

    #[test]
    fn fence_emit_uses_current_bucket() {
        // This test verifies that maybe_request_rerasters emits at the
        // CURRENT pane's bucket — exercises the fence emit pathway with a
        // pane wider than 800. Initial fence emit (Task 14 wires this) uses
        // the same path conceptually.
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(12), ready_at(800, 1));
        view.maybe_request_rerasters(200, 8); // pane_w_px=1600 → bucket 1600
        assert_eq!(view.pending_fence_actions.len(), 1);
        match view.pending_fence_actions.front() {
            Some(crate::action::Action::MermaidRenderRequest { target_width, .. }) => {
                assert!(*target_width >= 1200, "target_width should be ≥ 1200, got {target_width}");
            }
            _ => panic!("expected MermaidRenderRequest"),
        }
    }

    #[test]
    fn bucket_up_smoke_test() {
        // End-to-end: a Ready diagram at bucket 800, pane grows to 1600,
        // re-raster request emitted, completion handler runs, bucket
        // updated, image_generation bumped.
        use crate::action::Action;
        use std::sync::Arc;
        use image::{DynamicImage, RgbaImage};

        let mut view = SessionDetailView::new_for_tests();

        // 1. Seed Ready at bucket 800, generation 1.
        let img1 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.mermaid_registry.insert(
            MermaidId(99),
            MermaidState::Ready {
                image: img1,
                code: "graph TD\nA-->B".into(),
                rastered_at_bucket: 800,
                image_generation: 1,
            },
        );
        view.next_image_generation = 1;

        // 2. Pane grows to bucket 1600.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(99)));
        // Confirm the request is the expected fence Action variant.
        assert!(matches!(
            view.pending_fence_actions.front(),
            Some(Action::MermaidRenderRequest { .. })
        ));

        // 3. Worker completes (simulated).
        let img2 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(99), 1600, Ok(img2));

        // 4. Verify state.
        assert!(!view.in_flight_renders.contains(&MermaidId(99)));
        match view.mermaid_registry.get(&MermaidId(99)) {
            Some(MermaidState::Ready {
                rastered_at_bucket,
                image_generation,
                code,
                ..
            }) => {
                assert_eq!(*rastered_at_bucket, 1600);
                assert!(*image_generation > 1, "generation must bump");
                assert_eq!(code, "graph TD\nA-->B", "code retained across re-raster");
            }
            _ => panic!("expected Ready"),
        }

        // 5. Subsequent maybe_request_rerasters at the SAME bucket emits nothing.
        view.pending_fence_actions.clear();
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }
}

#[cfg(all(test, feature = "markdown"))]
mod invalidate_protocols_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use image::{DynamicImage, RgbaImage};
    use std::cell::RefCell;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        }
    }

    fn test_view() -> SessionDetailView {
        SessionDetailView::new(
            spur_acp::SessionId("test".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    // Obsolete: tested removed `inline_protocol` field on MermaidState::Ready.
    // Conceptual replacement: `ImageCache::invalidate_all` tests in Task 7.

    #[test]
    fn alt_v_is_inert_when_render_picker_is_none() {
        use crate::action::Action;
        use crate::views::session_detail::ViewId;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut view = test_view();
        view.set_render_picker(None);

        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        let action =
            <SessionDetailView as crate::views::View>::handle_key(&mut view, key, &test_ctx());

        assert!(
            !matches!(action, Some(Action::NavigateTo(ViewId::MermaidOverlay(_)))),
            "Alt-v must not navigate to mermaid overlay when picker is None, got {action:?}"
        );
    }

    #[test]
    fn alt_v_opens_overlay_when_render_picker_is_some() {
        use crate::action::Action;
        use crate::views::session_detail::ViewId;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut view = test_view();
        view.set_render_picker(Some(ratatui_image::picker::Picker::halfblocks()));

        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        let action =
            <SessionDetailView as crate::views::View>::handle_key(&mut view, key, &test_ctx());

        match action {
            Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
            other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod static_command_seeding_tests {
    use super::*;
    use spur_acp::{AgentConfig, CommandsConfig, DispatchKind, SessionId, StaticCommandDecl};
    use std::sync::Arc;

    #[test]
    fn session_view_constructor_seeds_static_commands_from_config() {
        let mut cfg = AgentConfig::with_defaults("codex");
        cfg.display.handle = Some("codex".into());
        cfg.commands = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            static_commands: vec![StaticCommandDecl {
                name: "compact".into(),
                description: "Compact history".into(),
                hint: None,
            }],
            ..Default::default()
        };
        let view = SessionDetailView::new(
            SessionId("test".into()),
            "codex".into(),
            "brain".into(),
            std::path::PathBuf::from("."),
            Arc::new(cfg),
            Vec::new(),
        );
        let names: Vec<_> = view
            .command_registry
            .list()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert!(
            names.contains(&"compact".to_string()),
            "static /compact should be visible at startup, got {names:?}"
        );
    }
}

#[cfg(test)]
mod cancel_state_tests {
    use super::*;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        }
    }

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn agent_msg_chunk_event(session: &spur_acp::SessionId) -> SpurEvent {
        let update = spur_acp::SessionUpdate::AgentMessageChunk(spur_acp::ContentChunk::new(
            spur_acp::ContentBlock::from("hi".to_string()),
        ));
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn turn_complete_event(session: &spur_acp::SessionId) -> SpurEvent {
        SpurEvent::now(SpurEventBody::TurnComplete {
            session: session.clone(),
        })
    }

    fn agent_session_ready_event(
        session: &spur_acp::SessionId,
        mode: spur_acp::CancelMode,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session.clone(),
            acp_session_id: "acp-1".into(),
            brain: "claude".into(),
            resumed: false,
            cancel_mode: mode,
            fs_unsafe: false,
            caps: None,
        })
    }

    fn tool_call_event(session: &spur_acp::SessionId, id: &str) -> SpurEvent {
        let tc = spur_acp::AcpToolCall::new(spur_acp::ToolCallId::new(id), "read");
        let update = spur_acp::SessionUpdate::ToolCall(tc);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn tool_call_update_event(session: &spur_acp::SessionId, id: &str) -> SpurEvent {
        let fields = agent_client_protocol::schema::ToolCallUpdateFields::new()
            .status(spur_acp::ToolCallStatus::InProgress);
        let tcu = spur_acp::AcpToolCallUpdate::new(spur_acp::ToolCallId::new(id), fields);
        let update = spur_acp::SessionUpdate::ToolCallUpdate(tcu);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn plan_event(session: &spur_acp::SessionId) -> SpurEvent {
        let plan = spur_acp::Plan::new(vec![spur_acp::PlanEntry::new(
            "step 1",
            spur_acp::PlanEntryPriority::Medium,
            spur_acp::PlanEntryStatus::InProgress,
        )]);
        let update = spur_acp::SessionUpdate::Plan(plan);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    #[test]
    fn new_view_has_no_stream_in_flight() {
        let v = make_view();
        assert!(!v.stream_in_flight);
        assert!(!v.cancelling_in_flight);
        assert!(v.cancel_mode.is_none());
    }

    #[test]
    fn chunk_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&agent_msg_chunk_event(&sid), &test_ctx());
        assert!(v.stream_in_flight);
    }

    #[test]
    fn tool_call_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&tool_call_event(&sid, "t1"), &test_ctx());
        assert!(
            v.stream_in_flight,
            "tool-first turn should arm stream_in_flight"
        );
    }

    #[test]
    fn tool_call_update_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&tool_call_update_event(&sid, "t1"), &test_ctx());
        assert!(
            v.stream_in_flight,
            "ToolCallUpdate should arm stream_in_flight"
        );
    }

    #[test]
    fn plan_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&plan_event(&sid), &test_ctx());
        assert!(
            v.stream_in_flight,
            "plan-first turn should arm stream_in_flight"
        );
    }

    #[test]
    fn esc_cancels_after_tool_first_update() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        v.handle_spur_event(&tool_call_event(&sid, "t1"), &test_ctx());
        assert!(v.stream_in_flight);

        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(
            matches!(action, Some(Action::CancelStream { .. })),
            "Esc after tool-first update should emit CancelStream, got {action:?}"
        );
        assert!(v.cancelling_in_flight);
    }

    #[test]
    fn turn_complete_clears_both_flags() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.stream_in_flight = true;
        v.cancelling_in_flight = true;
        v.handle_spur_event(&turn_complete_event(&sid), &test_ctx());
        assert!(!v.stream_in_flight);
        assert!(!v.cancelling_in_flight);
    }

    #[test]
    fn agent_session_ready_populates_cancel_mode() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(
            &agent_session_ready_event(&sid, spur_acp::CancelMode::AcpSoft),
            &test_ctx(),
        );
        assert_eq!(v.cancel_mode, Some(spur_acp::CancelMode::AcpSoft));
    }

    #[test]
    fn event_for_different_session_is_ignored() {
        let mut v = make_view();
        let other = spur_acp::SessionId("other".to_string());
        v.handle_spur_event(&agent_msg_chunk_event(&other), &test_ctx());
        assert!(!v.stream_in_flight);
    }

    use crate::action::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(key: KeyCode) -> KeyEvent {
        KeyEvent::new(key, KeyModifiers::NONE)
    }

    #[test]
    fn esc_with_stream_in_flight_emits_cancel_stream() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::CancelStream { .. })));
        assert!(v.cancelling_in_flight);
    }

    #[test]
    fn esc_when_already_cancelling_falls_through_to_navigate_back() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancelling_in_flight = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::NavigateBack)));
    }

    #[test]
    fn esc_without_stream_preserves_navigate_back() {
        let mut v = make_view();
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::NavigateBack)));
    }

    #[test]
    fn cancel_note_uses_acp_soft_text_when_mode_is_acp_soft() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Cancellation requested"),
            "expected AcpSoft message; got {last_text:?}"
        );
    }

    #[test]
    fn cancel_note_uses_process_kill_text_when_mode_is_process_kill() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::ProcessKill);
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Stopping agent"),
            "expected ProcessKill message; got {last_text:?}"
        );
    }

    #[test]
    fn cancel_note_generic_when_cancel_mode_unknown() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = None;
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Cancellation requested"),
            "expected generic fallback; got {last_text:?}"
        );
    }
}

#[cfg(test)]
mod tool_depth_tests {
    #[test]
    fn tool_depth_nested_two_levels() {
        use std::collections::HashMap;
        let mut tool_depth: HashMap<String, u8> = HashMap::new();
        tool_depth.insert("tc-root".into(), 0);

        let depth_1 = Some("tc-root")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        tool_depth.insert("tc-child".into(), depth_1);
        assert_eq!(depth_1, 1);

        let depth_2 = Some("tc-child")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth_2, 2);
    }

    #[test]
    fn tool_depth_unknown_parent_defaults_zero() {
        use std::collections::HashMap;
        let tool_depth: HashMap<String, u8> = HashMap::new();
        let depth = Some("tc-ghost")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth, 0);
    }

    #[test]
    fn tool_depth_caps_at_eight() {
        use std::collections::HashMap;
        let mut tool_depth: HashMap<String, u8> = HashMap::new();
        tool_depth.insert("tc-deep".into(), 8);
        let depth = Some("tc-deep")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth, 8);
    }
}

#[cfg(test)]
mod extract_tool_call_text_tests {
    use super::*;

    #[test]
    fn extract_tool_call_text_renders_diff_content() {
        use agent_client_protocol::schema::{Diff, ToolCallContent};
        let diff =
            Diff::new("src/foo.rs", "fn new_name() {}\n").old_text("fn old() {}\n".to_string());
        let content = vec![ToolCallContent::Diff(diff)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        assert!(out.contains("src/foo.rs"), "diff must include path");
        assert!(out.contains("-fn old"), "diff must include old-line prefix");
        assert!(
            out.contains("+fn new_name"),
            "diff must include new-line prefix"
        );
    }

    #[test]
    fn extract_tool_call_text_renders_terminal_placeholder() {
        use agent_client_protocol::schema::{Terminal, TerminalId, ToolCallContent};
        let term = Terminal::new(TerminalId::new("term-abc-123"));
        let content = vec![ToolCallContent::Terminal(term)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        assert!(out.contains("term-abc-123"), "placeholder must include id");
        assert!(out.starts_with("[terminal:"), "placeholder must be labeled");
    }

    #[test]
    fn extract_tool_call_text_truncates_long_diffs() {
        use agent_client_protocol::schema::{Diff, ToolCallContent};
        let big_new = "line\n".repeat(200);
        let diff = Diff::new("big.txt", big_new).old_text(String::new());
        let content = vec![ToolCallContent::Diff(diff)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        let line_count = out.lines().count();
        assert_eq!(
            line_count, 43,
            "expected 2 header + 40 body + 1 trailer, got {} lines",
            line_count
        );
        assert!(
            out.contains("160 more lines"),
            "must show exact truncated count: {}",
            out
        );
    }

    #[test]
    fn extract_tool_call_text_concatenates_multiple_entries() {
        use agent_client_protocol::schema::{Diff, Terminal, TerminalId, ToolCallContent};
        let diff_entry =
            ToolCallContent::Diff(Diff::new("a.rs", "y\n").old_text("x\n".to_string()));
        let term_entry = ToolCallContent::Terminal(Terminal::new(TerminalId::new("t-1")));
        let out = extract_tool_call_text(&[diff_entry, term_entry]).expect("should return Some");
        assert!(out.contains("a.rs"), "diff section must render");
        assert!(out.contains("+y"), "diff + line must render");
        assert!(
            out.contains("[terminal: t-1]"),
            "terminal placeholder must render after diff"
        );
    }

    #[test]
    fn extract_tool_call_text_returns_none_for_empty_content() {
        let content: Vec<spur_acp::ToolCallContent> = vec![];
        assert!(extract_tool_call_text(&content).is_none());
    }
}

#[cfg(test)]
mod composer_routing_tests {
    use super::*;
    use crate::action::Action;
    use crate::views::View;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SpurEvent, SpurEventBody};

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        }
    }

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
        v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
    }

    fn press_mod(v: &mut SessionDetailView, code: KeyCode, m: KeyModifiers) -> Option<Action> {
        v.handle_key(KeyEvent::new(code, m), &test_ctx())
    }

    fn test_ctx_with_plan() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::LazyLock<spur_core::PlanProjectionStore> =
            std::sync::LazyLock::new(|| {
                let mut store = spur_core::PlanProjectionStore::default();
                store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
                    session_id: spur_acp::SessionId("s".into()),
                    snapshot: Box::new(PlanSnapshot {
                        plan_id: "plan-1".into(),
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
                }));
                store
            });
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: &PLAN_PROJECTION,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        }
    }

    #[test]
    fn empty_emacs_j_scrolls_without_typing() {
        let mut v = make_view();
        assert!(v.input_bar_text_for_test().is_empty());
        let act = press(&mut v, KeyCode::Char('j'));
        assert!(
            v.input_bar_text_for_test().is_empty(),
            "empty bar must not type 'j'"
        );
        assert!(
            matches!(act, Some(Action::ScrollDown)),
            "expected ScrollDown, got {:?}",
            act
        );
    }

    #[test]
    fn non_empty_emacs_j_stays_in_composer() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("hello".into(), 5);
        let anchor_before = v.react_trace().anchor_for_tests();

        let act = press(&mut v, KeyCode::Char('j'));

        assert_eq!(v.input_bar_text_for_test(), "helloj");
        assert_eq!(v.react_trace().anchor_for_tests(), anchor_before);
        assert!(act.is_none());
    }

    #[test]
    fn non_empty_up_moves_composer_cursor() {
        let mut v = make_view();
        let text = "line1\nline2";
        v.input_bar_mut_for_test().set_text(text.into(), text.len());
        let cursor_before = v.input_bar_mut_for_test().cursor();
        assert_eq!(cursor_before, text.len());
        let anchor_before = v.react_trace().anchor_for_tests();

        let act = press(&mut v, KeyCode::Up);

        assert_eq!(
            v.react_trace().anchor_for_tests(),
            anchor_before,
            "trace must not scroll when composer has text"
        );
        let cursor_after = v.input_bar_mut_for_test().cursor();
        assert!(
            cursor_after < cursor_before,
            "cursor should move up in multiline composer, before={cursor_before}, after={cursor_after}"
        );
        assert!(act.is_none());
    }

    #[test]
    fn pending_permission_with_non_empty_composer_emits_grant() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("hello".into(), 5);
        v.push_permission("allow file write?", 60);

        let act = press(&mut v, KeyCode::Char('y'));

        assert_eq!(
            v.input_bar_text_for_test(),
            "hello",
            "permission key must not type into bar"
        );
        assert!(
            matches!(
                act,
                Some(Action::PermissionGrant(
                    crate::action::PermissionChoice::Allow
                ))
            ),
            "expected PermissionGrant(Allow), got {:?}",
            act
        );
    }

    #[test]
    fn non_empty_vim_normal_ctrl_p_recalls_history_not_paste() {
        let mut v = make_view();
        v.set_edit_mode(crate::components::input_bar::EditMode::Vim(
            crate::components::input_bar::VimMode::Normal,
        ));
        v.seed_input_history(vec![crate::input_history::InputHistoryEntry::new(
            crate::input_history::InputStateSnapshot::from_text("refactor the walker"),
        )]);
        v.input_bar_mut_for_test()
            .set_text("current draft".into(), 13);

        let act = press_mod(&mut v, KeyCode::Char('p'), KeyModifiers::CONTROL);

        assert_eq!(
            v.input_bar_text_for_test(),
            "refactor the walker",
            "Ctrl+P must recall history in Vim Normal, not paste"
        );
        assert!(act.is_none(), "history nav must not emit an action");
    }

    #[test]
    fn alt_v_without_render_picker_reaches_composer() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("x".into(), 1);
        let act = press_mod(&mut v, KeyCode::Char('v'), KeyModifiers::ALT);
        assert_eq!(
            v.input_bar_text_for_test(),
            "xv",
            "Alt+V must reach composer when render_picker is None"
        );
        assert!(act.is_none(), "composer typing must not emit action");
    }

    #[test]
    fn alt_p_noops_without_tracked_plan() {
        let mut v = make_view();
        let act = press_mod(&mut v, KeyCode::Char('p'), KeyModifiers::ALT);
        assert!(act.is_none(), "Alt+P must no-op without tracked plan");
    }

    #[test]
    fn alt_p_opens_plan_inspector_when_plan_is_tracked() {
        let mut v = make_view();
        let act = v.handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
            &test_ctx_with_plan(),
        );
        assert!(matches!(
            act,
            Some(Action::NavigateTo(ViewId::PlanInspector(_)))
        ));
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn alt_v_with_render_picker_navigates_to_overlay() {
        let mut v = make_view();
        v.set_render_picker(Some(ratatui_image::picker::Picker::halfblocks()));
        let act = press_mod(&mut v, KeyCode::Char('v'), KeyModifiers::ALT);
        match act {
            Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
            other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::LazyLock<spur_core::PlanProjectionStore> =
            std::sync::LazyLock::new(spur_core::PlanProjectionStore::new);
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: &PLAN_PROJECTION,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        }
    }

    fn prompt_dispatched_event(
        session: &spur_acp::SessionId,
        turn_kind: &str,
        continuations_count: usize,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::PromptDispatched {
            session: session.clone(),
            turn_kind: turn_kind.into(),
            continuations_count,
        })
    }

    fn continuation_dropped_event(delegation_id: &str) -> SpurEvent {
        SpurEvent::now(SpurEventBody::ContinuationDropped {
            delegation_id: delegation_id.into(),
            attempt: 1,
            brain_session: spur_acp::SessionId("test-brain-session".into()),
            reason: spur_acp::domain::continuation::DropReason::SessionSwap,
        })
    }

    fn delegation_completed_event(
        worker_session: &str,
        status: spur_acp::DelegationStatus,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::DelegationCompleted {
            worker_session: spur_acp::SessionId(worker_session.into()),
            status,
        })
    }

    #[test]
    fn new_view_defaults_cleared_false_and_no_ready_banner() {
        let view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        assert!(!view.is_cleared(), "new view must default cleared=false");
        assert!(
            view.ready_banner_text().is_none(),
            "new view must not start with a ready banner"
        );
    }

    #[test]
    fn reset_for_clear_wipes_conversation_state() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        // Seed state that reset_for_clear must wipe.
        view.tool_depth.insert("t1".to_string(), 2);
        #[cfg(feature = "markdown")]
        view.mermaid_registry.insert(
            crate::components::mermaid::MermaidId(1),
            crate::components::mermaid::MermaidState::Rendering,
        );

        view.reset_for_clear();

        assert!(view.tool_depth.is_empty(), "tool_depth must be cleared");
        // ReactTrace must be empty after reset — use whatever public
        // emptiness accessor exists on ReactTrace (grep
        // components/react_trace/mod.rs for `pub fn len\|is_empty\|entry_count`).
        // If no direct accessor, assert via rendered output in Task 10.
        // For now, assert the flag was set:
        assert!(view.is_cleared());
        assert_eq!(view.ready_banner_text(), Some(READY_BANNER_TEXT));
    }

    #[test]
    fn reset_for_clear_clears_header_status_fields() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        // Seed via existing public APIs.
        view.set_current_mode(Some("plan".into()));
        view.cost = 1.23;
        view.context_used = Some(1234);
        view.context_size = Some(200_000);
        view.auth_error = Some("auth failed".into());
        view.stream_in_flight = true;
        view.cancelling_in_flight = true;

        view.reset_for_clear();

        assert_eq!(view.cost, 0.0);
        assert_eq!(view.current_mode, None);
        assert_eq!(view.context_used, None);
        assert_eq!(view.context_size, None);
        assert_eq!(view.auth_error, None);
        assert!(!view.stream_in_flight);
        assert!(!view.cancelling_in_flight);
        // react_trace's mode mirror must also reset.
        assert_eq!(view.react_trace.current_mode(), None);
    }

    #[test]
    fn cleared_view_suppresses_force_save_draft() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();
        // Even with new text in the InputBar, force_save_draft must not
        // emit an Action keyed on the retired session_id.
        view.input_bar.set_text("new text".into(), 8);
        assert!(
            view.force_save_draft().is_none(),
            "cleared view must suppress force_save_draft"
        );
    }

    #[test]
    fn cleared_view_suppresses_draft_save_action() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();
        view.input_bar.set_text("new text".into(), 8);
        // Simulate a debounce trigger: set last_draft_change_at 600ms ago.
        view.test_set_last_draft_change(
            std::time::Instant::now() - std::time::Duration::from_millis(600),
        );
        assert!(
            view.draft_save_action().is_none(),
            "cleared view must suppress draft_save_action (debounce tick)"
        );
    }

    #[test]
    fn reset_for_clear_is_idempotent() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.react_trace.clear(); // normalize
        view.tool_depth.insert("seeded".into(), 1);
        view.reset_for_clear();
        let banner1 = view.ready_banner_text().map(str::to_string);
        view.reset_for_clear();
        let banner2 = view.ready_banner_text().map(str::to_string);
        assert_eq!(banner1, banner2);
        assert!(view.is_cleared());
        assert!(view.tool_depth.is_empty());
    }

    #[test]
    fn ready_banner_renders_when_cleared() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();

        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        let ctx = crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                <SessionDetailView as crate::views::View>::render(&mut view, f, f.area(), &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("Session cleared"),
            "ready banner text must appear. Rendered:\n{rendered}"
        );
    }

    #[test]
    fn reset_for_clear_wipes_draft_debounce_locals() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.last_persisted_draft = "stale".into();
        view.last_draft_change_at = Some(std::time::Instant::now());
        view.reset_for_clear();
        assert_eq!(view.last_persisted_draft, "");
        assert!(view.last_draft_change_at.is_none());
    }

    #[test]
    fn prompt_dispatched_continuation_only_pushes_think_entry() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(
            &prompt_dispatched_event(&sid, "continuation_only", 3),
            &test_ctx(),
        );
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Think));
        assert!(last.text.contains("Brain resuming with 3 worker results"));
    }

    #[test]
    fn prompt_dispatched_merged_pushes_think_entry() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&prompt_dispatched_event(&sid, "merged", 1), &test_ctx());
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Think));
        assert!(last
            .text
            .contains("Merging user message with 1 worker result"));
    }

    #[test]
    fn prompt_dispatched_user_only_is_no_op() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        let before = v.react_trace.entry_count();
        v.handle_spur_event(&prompt_dispatched_event(&sid, "user_only", 0), &test_ctx());
        assert_eq!(v.react_trace.entry_count(), before);
    }

    #[test]
    fn prompt_dispatched_different_session_is_ignored() {
        let mut v = make_view();
        let other = spur_acp::SessionId("other".into());
        let before = v.react_trace.entry_count();
        v.handle_spur_event(
            &prompt_dispatched_event(&other, "continuation_only", 2),
            &test_ctx(),
        );
        assert_eq!(v.react_trace.entry_count(), before);
    }

    #[test]
    fn continuation_dropped_pushes_observe_entry() {
        let mut v = make_view();
        v.handle_spur_event(&continuation_dropped_event("del-42"), &test_ctx());
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Observe { .. }));
        assert!(last.text.contains("Continuation dropped for del-42"));
    }

    #[test]
    fn delegation_completed_updates_delegate_status() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        // Seed a delegation request so the trace has a Delegate entry.
        v.handle_spur_event(
            &SpurEvent::now(SpurEventBody::DelegationRequested {
                from: sid.clone(),
                to_agent: "codex".into(),
                task: "fix bug".into(),
                request_id: "req-1".into(),
                delegation_plan: None,
                issue_id: None,
            }),
            &test_ctx(),
        );
        // Attach executor_id (simulating DelegationDispatched).
        v.handle_spur_event(
            &SpurEvent::now(SpurEventBody::DelegationDispatched {
                from: sid.clone(),
                request_id: "req-1".into(),
                executor_id: "exec-1".into(),
            }),
            &test_ctx(),
        );
        // Emit completion.
        v.handle_spur_event(
            &delegation_completed_event("exec-1", spur_acp::DelegationStatus::Success),
            &test_ctx(),
        );
        let entries = v.react_trace.entries_for_test();
        let delegate_entry = entries
            .iter()
            .find(|e| matches!(e.kind, TraceKind::Delegate { .. }))
            .unwrap();
        match &delegate_entry.kind {
            TraceKind::Delegate { status, .. } => assert_eq!(status, "done"),
            other => panic!("expected Delegate, got {:?}", other),
        }
    }
}
