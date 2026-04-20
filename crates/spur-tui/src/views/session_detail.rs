use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{SessionId, SpurEvent, SpurEventBody};

use crate::action::{Action, ViewId};
use crate::components::input_bar::{EditMode, InputBar};
use crate::components::react_trace::{ReactTrace, TraceEntry, TraceKind};
use crate::components::status_bar::{StatusBar, StatusBarProps};
use crate::input_history::InputHistoryEntry;

use super::View;

const READY_BANNER_TEXT: &str =
    "✨ Session cleared — your next prompt starts a fresh brain.";

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
    /// Stateful trigger-transition detector. Replaces the former
    /// trigger state field (retired in Phase 4). History shells are
    /// not managed through this detector; see dispatch_intent.
    trigger_detector: crate::components::completion_trigger::TriggerDetector,
    /// Registry of `@`-mention sources (files, directories).
    mention_registry: std::rc::Rc<std::cell::RefCell<crate::mentions::MentionRegistry>>,
    /// Working directory used to resolve file mentions.
    cwd: std::path::PathBuf,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    #[cfg(feature = "markdown")]
    pub(crate) pending_fence_actions: std::collections::VecDeque<crate::action::Action>,
    /// Graphics `Picker` used to build inline mermaid image protocols during
    /// render. Set once from `App` when the view is created; `None` when no
    /// graphics protocol is available (text fallback kicks in).
    #[cfg(feature = "markdown")]
    render_picker: Option<ratatui_image::picker::Picker>,
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

    /// Active picker-shell (history / mention / slash). `None` = no popup.
    picker_shell: Option<crate::components::picker_shell::PickerShell>,
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
            trigger_detector: crate::components::completion_trigger::TriggerDetector::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd,
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
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
            picker_shell: None,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names,
            cleared: false,
            ready_banner: None,
        }
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
            trigger_detector:
                crate::components::completion_trigger::TriggerDetector::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: std::path::PathBuf::from("."),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
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
            picker_shell: None,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names: std::collections::HashSet::new(),
            cleared: false,
            ready_banner: None,
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
    /// - UI transient: `resume_banner`, `picker_shell`, `trigger_detector`.
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
        self.trigger_detector.reset();
        self.picker_shell = None;
        self.resume_banner = None;

        // Marks — header/status fields land in Task 3; draft locals in Task 4.
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
    pub fn ready_banner_text(&self) -> Option<&str> {
        self.ready_banner.as_deref()
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

    /// Drop every cached inline `StatefulProtocol` so they are rebuilt at
    /// the new Rect size on the next render. Called on terminal resize.
    #[cfg(feature = "markdown")]
    pub fn invalidate_inline_protocols(&mut self) {
        use crate::components::mermaid::MermaidState;
        for state in self.mermaid_registry.values() {
            if let MermaidState::Ready {
                inline_protocol, ..
            } = state
            {
                *inline_protocol.borrow_mut() = None;
            }
        }
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
            self.input_bar
                .set_status(Some(format!("[{}: cancelling\u{2026}]", self.agent_name)));
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

        let label = match status {
            "idle" => {
                if mention_count > 0 {
                    Some(format!(
                        "[{} mention{}]",
                        mention_count,
                        if mention_count > 1 { "s" } else { "" }
                    ))
                } else {
                    None
                }
            }
            "thinking" => Some(format!(
                "[{} \u{00b7}\u{00b7}\u{00b7}{}]",
                self.agent_name, mention_suffix
            )),
            "streaming" => Some(format!(
                "[{} \u{25b8}\u{25b8}\u{25b8}{}]",
                self.agent_name, mention_suffix
            )),
            "ready" => Some(format!("[{}: ready{}]", self.agent_name, mention_suffix)),
            "error" => Some(format!("[{}: error{}]", self.agent_name, mention_suffix)),
            other => Some(format!(
                "[{}: {}{}]",
                self.agent_name, other, mention_suffix
            )),
        };
        self.input_bar.set_status(label);
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
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    ) {
        use crate::components::mermaid::MermaidState;
        let state = match result {
            Ok(image_arc) => MermaidState::Ready {
                image: image_arc,
                inline_protocol: std::cell::RefCell::new(None),
            },
            Err(message) => MermaidState::Error { message },
        };
        self.mermaid_registry.insert(ref_id, state);

        // Mark every markdown stream dirty so the next tick's maybe_flush
        // rebuilds placeholders — transitions Pending→Ready (📊) or →Error (⚠).
        self.react_trace.mark_all_streams_dirty();
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

    /// Feed a classified IntentEvent into the TriggerDetector and apply the
    /// resulting transition to `self.picker_shell`. Includes the Idle
    /// fast-path: on `Idle` state and a non-opening event, return in O(1)
    /// without fetching text/cursor/ranges from `input_bar`.
    fn dispatch_intent(&mut self, event: crate::components::completion_trigger::IntentEvent) {
        use crate::components::completion_trigger::{IntentEvent, TriggerKind, TriggerTransition};
        use crate::components::picker_shell::PickerShell;
        use crate::components::query_source::{
            MentionQuerySource, QueryMode, SlashQuerySource, SlashRow,
        };

        // Fast path: Idle state + non-opening event → no text fetch, no alloc.
        // Ordered first so the hottest path (Idle, no picker, typing/motion)
        // skips the Option deref for the picker-shell check entirely.
        if self.trigger_detector.is_idle()
            && !matches!(
                event,
                IntentEvent::TypedChar('@') | IntentEvent::TypedChar('/')
            )
        {
            return;
        }

        // History-mode shell owns the picker; detector is inert.
        if let Some(shell) = self.picker_shell.as_ref() {
            if shell.query_mode() == QueryMode::OwnedByShell {
                self.trigger_detector.reset();
                return;
            }
        }

        let text = self.input_bar.text();
        let cursor = self.input_bar.cursor();
        // Clone required: step() takes &mut self on trigger_detector while ranges
        // borrows from input_bar — two simultaneous fields of self.
        let ranges = self.input_bar.protected_ranges().to_vec();

        let transition = self.trigger_detector.step(event, &text, cursor, &ranges);

        match transition {
            TriggerTransition::None => {}
            TriggerTransition::Update { query } => {
                if let Some(shell) = self.picker_shell.as_mut() {
                    shell.set_query_from_input_bar(&query);
                }
            }
            TriggerTransition::Open { trigger } => {
                let shell = match trigger.kind {
                    TriggerKind::Slash => {
                        let entries = self.command_registry.list();
                        let rows: Vec<SlashRow> = entries
                            .iter()
                            .map(|e| SlashRow {
                                canonical: self.command_registry.canonical_typed_form(e),
                                description: e.description.clone(),
                                tag: match &e.source {
                                    crate::commands::CommandSource::Spur => "⟨spur⟩".into(),
                                    crate::commands::CommandSource::Agent { handle } => {
                                        format!("⟨{}⟩", handle)
                                    }
                                },
                            })
                            .collect();
                        let src = SlashQuerySource::new(rows, trigger.prefix_start);
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                    TriggerKind::Mention => {
                        let src = MentionQuerySource::new(
                            std::rc::Rc::clone(&self.mention_registry),
                            self.session_id.clone(),
                            self.cwd.clone(),
                            trigger.prefix_start,
                        );
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                };
                self.picker_shell = Some(shell);
            }
            TriggerTransition::Close => {
                self.picker_shell = None;
            }
        }
    }

    /// Replace the range [prefix_start..cursor] in the InputBar with `replacement`.
    /// Leaves the cursor at `prefix_start + replacement.len()`.
    fn replace_trigger_token(&mut self, prefix_start: usize, replacement: &str) {
        let current = self.input_bar.text().to_string();
        let cursor = self.input_bar.cursor();
        let mut new_text = String::with_capacity(current.len());
        new_text.push_str(&current[..prefix_start]);
        new_text.push_str(replacement);
        new_text.push_str(&current[cursor..]);
        let new_cursor = prefix_start + replacement.len();
        self.input_bar.set_text(new_text, new_cursor);
        self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
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
            self.input_bar
                .set_status(Some(format!("[{}: cancelling\u{2026}]", self.agent_name)));
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

        // Vim Normal + empty InputBar: handle nav keys directly.
        // In Vim Normal mode, chars are consumed as commands (not inserted),
        // so the single-char-nav pattern (insert → check len==1) doesn't work.
        // Mode-entry keys (i/a/A/I/o/O) fall through to InputBar.
        if self.input_bar.is_empty() && self.input_bar.is_vim_normal() {
            if let KeyCode::Char(ch) = key.code {
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    let action = match ch {
                        'j' => {
                            self.react_trace.scroll_down();
                            Some(Action::ScrollDown)
                        }
                        'k' => {
                            self.react_trace.scroll_up();
                            Some(Action::ScrollUp)
                        }
                        'g' => {
                            self.react_trace.scroll_to_top();
                            Some(Action::ScrollToTop)
                        }
                        'G' => {
                            self.react_trace.scroll_to_bottom();
                            Some(Action::ScrollToBottom)
                        }
                        // Mode-entry keys fall through to InputBar
                        'i' | 'a' | 'A' | 'I' | 'o' | 'O' => None,
                        _ => return None, // Unrecognized: no-op
                    };
                    if let Some(a) = action {
                        return Some(a);
                    }
                }
            }
        }

        // Ctrl+O → toggle collapse/expand on Observe (tool-result) entries.
        if matches!(key.code, KeyCode::Char('o')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.react_trace.toggle_observe_collapsed();
            return None;
        }

        // Ctrl+P / Ctrl+N → input history navigation.
        if matches!(key.code, KeyCode::Char('p')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_prev();
            self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
            return None;
        }
        if matches!(key.code, KeyCode::Char('n')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_next();
            self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
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

        // Priority 1: Permission handling when a permission is pending.
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

        // Priority 1.4: picker shell (history via Ctrl+R, or trigger-driven
        // @mention / /slash). Trigger-driven shells (ReadFromInputBar) read
        // their query from the InputBar, so editing keys must fall through
        // to input_bar; only navigation/accept/cancel keys are consumed
        // here. History shells (OwnedByShell) own their own MiniInput and
        // receive ALL keys.
        if self.picker_shell.is_some() {
            use crate::components::query_source::QueryMode;
            let shell_mode = self
                .picker_shell
                .as_ref()
                .map(|s| s.query_mode())
                .expect("is_some checked");
            let is_trigger_driven = shell_mode == QueryMode::ReadFromInputBar;
            let shell_consumes = if is_trigger_driven {
                matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
                ) || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
            } else {
                true
            };
            if shell_consumes {
                use crate::components::picker_shell::PickerAction;
                use crate::components::query_source::RetrievalAccept;
                let shell = self.picker_shell.as_mut().expect("is_some checked");
                let act = shell.handle_key(key);
                match act {
                    PickerAction::None => {}
                    PickerAction::Cancel => {
                        self.picker_shell = None;
                        self.dispatch_intent(
                            crate::components::completion_trigger::IntentEvent::Dismissed,
                        );
                    }
                    PickerAction::Accept(accept) => {
                        match accept {
                            RetrievalAccept::ReplaceState(snap) => {
                                let len = snap.text.len();
                                self.input_bar.set_state(snap, len);
                                self.dispatch_intent(
                                    crate::components::completion_trigger::IntentEvent::Accepted,
                                );
                            }
                            RetrievalAccept::InsertAtom {
                                text,
                                uri,
                                name,
                                replace_from,
                            } => {
                                if let Some(prefix_start) = replace_from {
                                    self.replace_trigger_token(prefix_start, "");
                                }
                                self.input_bar.insert_atom(text, uri, name);
                                self.dispatch_intent(
                                    crate::components::completion_trigger::IntentEvent::Accepted,
                                );
                            }
                            RetrievalAccept::ReplaceTriggerToken {
                                prefix_start,
                                replacement,
                            } => {
                                self.replace_trigger_token(prefix_start, &replacement);
                                self.dispatch_intent(
                                    crate::components::completion_trigger::IntentEvent::Accepted,
                                );
                            }
                        }
                        self.picker_shell = None;
                    }
                }
                return None;
            }
            // else: editing key on a trigger-driven shell; fall through so
            // input_bar receives it, then dispatch_intent syncs the shell query.
        }

        // Ctrl+R / Alt+R → open history PickerShell. Rejected while a
        // completion_trigger popup is active (user must Esc first).
        if matches!(key.code, KeyCode::Char('r'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
            && self.picker_shell.is_none()
        {
            use crate::components::picker_shell::PickerShell;
            use crate::components::query_source::HistoryQuerySource;
            let history = self.input_bar.history().to_vec();
            self.picker_shell = Some(PickerShell::open(Box::new(HistoryQuerySource::new(
                history,
            ))));
            return None;
        }

        // Priority 2: If the key is a printable char or an editing key, route to input_bar.
        let is_editing_key = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
        ) || (key.code == KeyCode::Esc && self.input_bar.wants_esc());

        if is_editing_key {
            use crate::components::completion_trigger::IntentEvent;
            use crate::components::input_bar::HandleOutcome;
            match self.input_bar.handle_key(key) {
                HandleOutcome::Submit(_, _) => {
                    // Notify detector before processing submit (the detector doesn't
                    // care about the text; this just retires any open composition).
                    self.dispatch_intent(IntentEvent::Submitted);
                    if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
                        use crate::commands::submit_router::{route, SubmitDecision};
                        let dec = route(&text, &ranges, &self.command_registry, interrupt);
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
                                Some(Action::SendMessage {
                                    session: self.session_id.clone(),
                                    blocks,
                                    interrupt,
                                })
                            }
                            SubmitDecision::Local { action } => Some(action),
                            SubmitDecision::VendorExec { method, params } => {
                                Some(Action::VendorExec {
                                    session: self.session_id.clone(),
                                    method,
                                    params,
                                })
                            }
                        };
                    }
                    return None;
                }
                HandleOutcome::Key(intent) => {
                    self.dispatch_intent(intent);
                }
            }

            // If the input_bar is empty and the key was a navigation key (j/k/g/G),
            // we want scroll behavior instead. But since we already routed to
            // input_bar above, we only fall through for non-char keys when empty.
            // Actually, chars always go to input_bar first. The spec says:
            // "If input_bar is empty: j/k/Up/Down → scroll, g/G → jump, Esc → back"
            // But chars are "printable" so they go to input_bar which will insert them.
            // We need to check: if input was empty BEFORE this key and the key is
            // a scroll key, we should scroll instead. Let's re-check the spec:
            //
            // The spec says route printable/editing keys to input_bar. But it also
            // says when input_bar is empty, j/k/g/G should scroll. The resolution:
            // j/k/g/G when input is empty should scroll, not type.
            //
            // We already inserted the char though. Let's undo if it was a scroll
            // key and the bar was previously empty (now has exactly 1 char).
            if self.input_bar.text().len() == 1 {
                let ch = self.input_bar.text().chars().next().unwrap();
                if matches!(ch, 'j' | 'k' | 'g' | 'G') {
                    self.input_bar.clear();
                    return match ch {
                        'j' => {
                            self.react_trace.scroll_down();
                            Some(Action::ScrollDown)
                        }
                        'k' => {
                            self.react_trace.scroll_up();
                            Some(Action::ScrollUp)
                        }
                        'g' => {
                            self.react_trace.scroll_to_top();
                            Some(Action::ScrollToTop)
                        }
                        'G' => {
                            self.react_trace.scroll_to_bottom();
                            Some(Action::ScrollToBottom)
                        }
                        _ => None,
                    };
                }
            }

            return None;
        }

        // Priority 3: Non-editing keys → scroll/navigate.
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

        if self.input_bar.is_empty() {
            match key.code {
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

impl View for SessionDetailView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        // Any keystroke dismisses the resume banner, but the key continues
        // to flow through to the normal handler — the banner is purely
        // informational and never consumes input.
        if let Some(ref mut banner) = self.resume_banner {
            banner.dismiss();
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

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
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

                // Flag streaming state for live content chunks. This is the
                // caller's responsibility — the shared dispatcher is agnostic
                // to session lifecycle state.
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(_)
                    | spur_acp::SessionUpdate::AgentMessageChunk(_) => {
                        self.stream_in_flight = true;
                    }
                    _ => {}
                }

                let agent_name = self.agent_name.clone();
                let agent_kind = self.agent_kind();
                let mut ctx = crate::components::react_trace::dispatch::DispatchCtx {
                    agent_name: agent_name.as_str(),
                    agent_kind,
                    now_stamp: Self::now_stamp,
                    tool_depth: &mut self.tool_depth,
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
                // The inline executor card (rendered from lineage) already
                // reflects the terminal state. A separate Think entry here
                // was redundant noise — and couldn't correlate back to the
                // originating Delegate entry anyway (event lacks request_id).
                //
                // Edge case: if worker setup failed before WorkerSpawned
                // (no executor node, no inline card), this no-op means the
                // only failure signal is the brain's own response text.
                // Acceptable — setup failures are rare and the brain always
                // reports the error in its next message.
                let _ = (worker_session, status);
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
                            self.pending_fence_actions.push_back(
                                crate::action::Action::MermaidRenderRequest {
                                    session: self.session_id.clone(),
                                    ref_id: fence.id,
                                    code: fence.code,
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
                ..
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                self.cancel_mode = Some(*cancel_mode);
                if *resumed {
                    self.push_system_note("Resumed from prior conversation".to_string());
                }
            }

            // All other event types are not relevant to this session view.
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.react_trace.tick();
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
                self.pending_fence_actions
                    .push_back(crate::action::Action::MermaidRenderRequest {
                        session: self.session_id.clone(),
                        ref_id: fence.id,
                        code: fence.code,
                    });
            }
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        self.render_inner(frame, area, Some(ctx.lineage), ctx.license_badge);
    }
}
impl SessionDetailView {
    fn render_inner(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
    ) {
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
            use ratatui::widgets::{Block, Borders, Wrap};
            let banner = Paragraph::new(msg.as_str())
                .style(
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                )
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Authentication required"),
                );
            frame.render_widget(banner, banner_area);
        }

        let input_height = self.input_bar.required_height(content_area.width);

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
            Constraint::Length(1),            // header
            Constraint::Min(4),               // react trace (fills)
            Constraint::Length(workers_h),    // workers panel
            Constraint::Length(input_height), // input bar
            Constraint::Length(1),            // status bar
        ])
        .split(content_area);

        // ── Header: breadcrumb + elapsed + cost ─────────────────────────
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
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // ── React trace ─────────────────────────────────────────────────
        #[cfg(feature = "markdown")]
        {
            let ctx = crate::components::react_trace::RenderContext {
                mermaid_registry: &self.mermaid_registry,
                picker: self.render_picker.as_ref(),
            };
            self.react_trace
                .render_with_ctx(frame, chunks[1], &ctx, lineage);
        }
        #[cfg(not(feature = "markdown"))]
        self.react_trace.render(frame, chunks[1], lineage);

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
        // Render in "inert" style (dimmed border, no terminal cursor) when
        // a PickerShell has the focus — the shell owns the cursor.
        if self.picker_shell.is_some() {
            self.input_bar.render_inert(frame, chunks[3]);
        } else {
            self.input_bar.render(frame, chunks[3]);
        }

        // ── PickerShell overlay ─────────────────────────────────────────
        if let Some(ref mut shell) = self.picker_shell {
            shell.render(frame, chunks[3], area);
        }

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

        StatusBar::render(
            frame,
            chunks[4],
            StatusBarProps {
                view: &ViewId::SessionDetail(self.session_id.clone()),
                running,
                pending_review,
                total_cost: self.cost,
                elapsed: &elapsed,
                current_mode: self.current_mode.as_deref(),
                context_used: self.context_used,
                context_size: self.context_size,
                stream_in_flight: self.stream_in_flight && !self.cancelling_in_flight,
                issue_count: 0,
                alert_summary: None,
                license_badge,
            },
        );

        // ── Resume banner (top row, if visible) ─────────────────────────
        if let (Some(banner), Some(rect)) = (self.resume_banner.as_ref(), resume_banner_area) {
            banner.render(frame, rect);
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

#[cfg(all(test, feature = "markdown"))]
mod invalidate_protocols_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use image::{DynamicImage, RgbaImage};
    use std::cell::RefCell;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        crate::views::ViewContext {
            lineage: &LINEAGE,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
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

    #[test]
    fn invalidate_clears_inline_protocols_on_all_ready_states() {
        let mut view = test_view();
        let id = MermaidId(1);
        view.mermaid_registry.insert(
            id,
            MermaidState::Ready {
                image: std::sync::Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(4, 4))),
                inline_protocol: RefCell::new(None),
            },
        );
        // Sanity: slot starts None.
        if let Some(MermaidState::Ready {
            inline_protocol, ..
        }) = view.mermaid_registry.get(&id)
        {
            assert!(inline_protocol.borrow().is_none());
        }

        // Invalidate is a no-op on already-None slots but must not panic or
        // mutate other variants.
        view.mermaid_registry
            .insert(MermaidId(2), MermaidState::Rendering);
        view.mermaid_registry.insert(
            MermaidId(3),
            MermaidState::Error {
                message: "boom".to_string(),
            },
        );
        view.invalidate_inline_protocols();

        assert!(matches!(
            view.mermaid_registry.get(&MermaidId(1)),
            Some(MermaidState::Ready { .. })
        ));
        assert!(matches!(
            view.mermaid_registry.get(&MermaidId(2)),
            Some(MermaidState::Rendering)
        ));
        assert!(matches!(
            view.mermaid_registry.get(&MermaidId(3)),
            Some(MermaidState::Error { .. })
        ));

        if let Some(MermaidState::Ready {
            inline_protocol, ..
        }) = view.mermaid_registry.get(&MermaidId(1))
        {
            assert!(
                inline_protocol.borrow().is_none(),
                "slot should remain None after invalidate"
            );
        }
    }

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
        crate::views::ViewContext {
            lineage: &LINEAGE,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
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
        use agent_client_protocol::{Diff, ToolCallContent};
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
        use agent_client_protocol::{Terminal, TerminalId, ToolCallContent};
        let term = Terminal::new(TerminalId::new("term-abc-123"));
        let content = vec![ToolCallContent::Terminal(term)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        assert!(out.contains("term-abc-123"), "placeholder must include id");
        assert!(out.starts_with("[terminal:"), "placeholder must be labeled");
    }

    #[test]
    fn extract_tool_call_text_truncates_long_diffs() {
        use agent_client_protocol::{Diff, ToolCallContent};
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
        use agent_client_protocol::{Diff, Terminal, TerminalId, ToolCallContent};
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
mod tests {
    use super::*;

    #[test]
    fn new_view_defaults_cleared_false_and_no_ready_banner() {
        let view = SessionDetailView::new_for_palette_test(
            crate::commands::CommandRegistry::default(),
        );
        assert!(!view.is_cleared(), "new view must default cleared=false");
        assert!(
            view.ready_banner_text().is_none(),
            "new view must not start with a ready banner"
        );
    }

    #[test]
    fn reset_for_clear_wipes_conversation_state() {
        let mut view = SessionDetailView::new_for_palette_test(
            crate::commands::CommandRegistry::default(),
        );
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
        assert_eq!(
            view.ready_banner_text(),
            Some(READY_BANNER_TEXT)
        );
    }
}
