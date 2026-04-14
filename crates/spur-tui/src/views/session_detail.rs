use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{DelegationStatus, SessionId, SpurEvent, SpurEventBody};

use crate::action::{Action, ViewId};
use crate::components::input_bar::InputBar;
use crate::components::react_trace::{ReactTrace, TraceEntry, TraceKind};
use crate::components::status_bar::{StatusBar, StatusBarProps};

use super::View;

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
    /// Autocomplete popup for `/` slash-commands (and later `@` mentions).
    /// Wrapped in `RefCell` because `View::render` takes `&self` but the
    /// popup needs `&mut` access for its internal `ListState`.
    completion_popup: std::cell::RefCell<crate::components::completion_popup::CompletionPopup>,
    /// Currently active popup trigger (if any), derived from the InputBar
    /// text + cursor.
    active_trigger: Option<crate::components::completion_trigger::Trigger>,
    /// Registry of `@`-mention sources (files, directories).
    mention_registry: crate::mentions::MentionRegistry,
    /// Working directory used to resolve file mentions.
    cwd: std::path::PathBuf,
    /// Mention hits currently shown in the popup, parallel to popup rows.
    /// Used on accept to retrieve the URI/display name.
    active_mention_hits: Vec<crate::mentions::MentionEntry>,
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
}

impl SessionDetailView {
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
    ) -> Self {
        let command_registry = crate::commands::CommandRegistry::from_configs(
            std::slice::from_ref(&*agent_cfg),
        );
        Self {
            session_id,
            agent_name,
            role,
            agent_cfg,
            react_trace: ReactTrace::new(),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion_popup: std::cell::RefCell::new(
                crate::components::completion_popup::CompletionPopup::new(),
            ),
            active_trigger: None,
            mention_registry: crate::mentions::MentionRegistry::new(),
            cwd,
            active_mention_hits: Vec::new(),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
        }
    }

    /// Show the resume banner for an auto-resumed session. Called by App on
    /// startup after reading session metadata.
    pub fn show_resume_banner(&mut self, title: String, quit_ago: String) {
        self.resume_banner = Some(crate::components::resume_banner::ResumeBanner::new(
            title, quit_ago,
        ));
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
        self.last_persisted_draft = draft.to_string();
    }

    /// Current text content of the InputBar (read-only accessor for tests).
    pub fn input_bar_text(&self) -> &str {
        self.input_bar.text()
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
            if let MermaidState::Ready { inline_protocol, .. } = state {
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

    /// Format elapsed time since view was opened.
    fn elapsed(&self) -> String {
        crate::components::format_elapsed(self.started_at)
    }

    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, status: &str) {
        let label = match status {
            "idle" => None,
            "thinking" => Some(format!("[{} \u{00b7}\u{00b7}\u{00b7}]", self.agent_name)),
            "streaming" => Some(format!("[{} \u{25b8}\u{25b8}\u{25b8}]", self.agent_name)),
            "ready" => Some(format!("[{}: ready]", self.agent_name)),
            "error" => Some(format!("[{}: error]", self.agent_name)),
            other => Some(format!("[{}: {}]", self.agent_name, other)),
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
            kind: TraceKind::Observe,
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

    fn refresh_popup(&mut self) {
        use crate::commands::fuzzy;
        use crate::components::completion_popup::PopupRow;
        use crate::components::completion_trigger::{detect, TriggerKind};

        let text = self.input_bar.text();
        let cursor = self.input_bar.cursor();
        let trig = detect(text, cursor);
        self.active_trigger = trig.clone();

        match trig {
            Some(t) if t.kind == TriggerKind::Slash => {
                let entries = self.command_registry.list();
                let ranked = fuzzy::rank(&entries, &t.query);
                let rows: Vec<PopupRow> = ranked
                    .iter()
                    .map(|e| PopupRow {
                        label: self.command_registry.canonical_typed_form(e),
                        description: e.description.clone(),
                        source_tag: match &e.source {
                            crate::commands::CommandSource::Spur => "⟨spur⟩".into(),
                            crate::commands::CommandSource::Agent { handle } => {
                                format!("⟨{}⟩", handle)
                            }
                        },
                    })
                    .collect();
                self.completion_popup.borrow_mut().set_rows(rows);
                self.active_mention_hits.clear();
            }
            Some(t) if t.kind == TriggerKind::Mention => {
                let hits = self.mention_registry.query(
                    &self.session_id,
                    &self.cwd,
                    &t.query,
                    20,
                );
                let rows: Vec<PopupRow> = hits
                    .iter()
                    .map(|m| {
                        let icon = match m.kind {
                            crate::mentions::MentionKind::Directory => "\u{1F4C1}",
                            crate::mentions::MentionKind::File => "\u{1F4C4}",
                        };
                        PopupRow {
                            label: format!("{} @{}", icon, m.display),
                            description: String::new(),
                            source_tag: String::new(),
                        }
                    })
                    .collect();
                self.completion_popup.borrow_mut().set_rows(rows);
                self.active_mention_hits = hits;
            }
            _ => {
                self.completion_popup.borrow_mut().set_rows(Vec::new());
                self.active_mention_hits.clear();
            }
        }
    }

    fn popup_open(&self) -> bool {
        self.active_trigger.is_some() && !self.completion_popup.borrow().is_empty()
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
    }

    fn accept_completion(&mut self) -> Option<crate::action::Action> {
        use crate::components::completion_trigger::TriggerKind;

        let trig = self.active_trigger.clone()?;
        let idx = self.completion_popup.borrow().selected()?;
        let rows = self.completion_popup.borrow().rows().to_vec();
        let row = rows.get(idx)?.clone();

        match trig.kind {
            TriggerKind::Slash => {
                // row.label is the canonical typed form (e.g. "/help" or "/claude:help").
                let insertion = format!("{} ", row.label);
                self.replace_trigger_token(trig.prefix_start, &insertion);
                self.active_trigger = None;
                self.completion_popup.borrow_mut().set_rows(Vec::new());
                None
            }
            TriggerKind::Mention => {
                let idx = self.completion_popup.borrow().selected()?;
                let hit = self.active_mention_hits.get(idx)?.clone();

                // Clear the `@query` range, then insert the atom at the vacated position.
                self.replace_trigger_token(trig.prefix_start, "");
                let atom = format!("@{}", hit.display);
                self.input_bar.insert_atom(atom, hit.uri, hit.display);
                self.active_trigger = None;
                self.completion_popup.borrow_mut().set_rows(Vec::new());
                self.active_mention_hits.clear();
                None
            }
        }
    }

    /// Build (error_ids, pending_ids) sets from the mermaid registry for use
    /// in constructing a `StateLookup`.
    #[cfg(feature = "markdown")]
    fn build_state_lookup_sets(&self) -> (
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
    ) {
        use crate::components::mermaid::MermaidState;
        let mut errors = std::collections::HashSet::new();
        let mut pending = std::collections::HashSet::new();
        for (id, state) in &self.mermaid_registry {
            match state {
                MermaidState::Error { .. } => { errors.insert(*id); }
                MermaidState::Pending { .. } | MermaidState::Rendering => { pending.insert(*id); }
                MermaidState::Ready { .. } => {}
            }
        }
        (errors, pending)
    }
}

impl SessionDetailView {
    fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
        // Dismiss the auth banner on any keystroke (before any further routing).
        // The mode-toggle binding below still fires because the action is
        // dispatched regardless.
        if self.auth_error.is_some() {
            self.auth_error = None;
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

        #[cfg(feature = "markdown")]
        if matches!(key.code, KeyCode::Char('v'))
            && key.modifiers.contains(KeyModifiers::ALT)
            && self.render_picker.is_some()
        {
            return Some(Action::NavigateTo(ViewId::MermaidOverlay(self.session_id.clone())));
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

        // Priority 1.5: popup is open — route navigation/accept/dismiss keys.
        if self.popup_open() {
            match key.code {
                KeyCode::Up => {
                    self.completion_popup.borrow_mut().select_prev();
                    return None;
                }
                KeyCode::Down => {
                    self.completion_popup.borrow_mut().select_next();
                    return None;
                }
                KeyCode::Esc => {
                    self.active_trigger = None;
                    self.completion_popup.borrow_mut().set_rows(Vec::new());
                    return None;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    return self.accept_completion();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.active_trigger = None;
                    self.completion_popup.borrow_mut().set_rows(Vec::new());
                    return None;
                }
                _ => { /* fall through to editing */ }
            }
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
        );

        if is_editing_key {
            if self.input_bar.handle_key(key).is_some() {
                // Enter fired. Take the capture and route through SubmitRouter.
                if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
                    use crate::commands::submit_router::{route, SubmitDecision};
                    let dec = route(&text, &ranges, &self.command_registry, interrupt);
                    return match dec {
                        SubmitDecision::Empty => None,
                        SubmitDecision::Send { blocks, interrupt } => {
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

            // Key was an ordinary edit (insert/delete/arrow). Re-evaluate popup state.
            self.refresh_popup();

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
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
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

    fn handle_spur_event(&mut self, event: &SpurEvent) {
        match &event.body {
            SpurEventBody::AgentNotification { session, notification } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                // Read-only mirror of session-scoped state (mode, usage,
                // available commands). Handled before the trace-rendering
                // match so we always capture it regardless of whether a
                // display arm fires below.
                crate::app::apply_session_update(self, &notification.update);
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                        if let Some(text) = extract_text(chunk) {
                            if !text.is_empty() {
                                self.react_trace.append_think(text, Self::now_stamp());
                            }
                        }
                    }
                    spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let Some(text) = extract_text(chunk) {
                            if !text.is_empty() {
                                let prev_kind = self
                                    .react_trace
                                    .last_entry_kind_name()
                                    .unwrap_or("none");
                                let will_continue = prev_kind == "agent_message";
                                tracing::debug!(
                                    streaming_probe = true,
                                    site = "D_trace_append",
                                    text_len = text.len(),
                                    prev_entry_kind = prev_kind,
                                    will_continue = will_continue,
                                    session = %self.session_id,
                                    "about to append_message"
                                );
                                self.react_trace.append_message(text, &self.agent_name, Self::now_stamp());
                            }
                        }
                    }
                    spur_acp::SessionUpdate::ToolCall(tc) => {
                        let null = serde_json::Value::Null;
                        let args = format_tool_args(tc.raw_input.as_ref().unwrap_or(&null));
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Act {
                                tool: tc.title.clone(),
                                args,
                            },
                            text: String::new(),
                            timestamp: Self::now_stamp(),
                            #[cfg(feature = "markdown")]
                            markdown: None,
                        });
                    }
                    spur_acp::SessionUpdate::ToolCallUpdate(tcu) => {
                        let null = serde_json::Value::Null;
                        let text = format_observe_output(tcu.fields.raw_output.as_ref().unwrap_or(&null));
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Observe,
                            text,
                            timestamp: Self::now_stamp(),
                            #[cfg(feature = "markdown")]
                            markdown: None,
                        });
                    }
                    spur_acp::SessionUpdate::Plan(plan) => {
                        let text = plan.entries.iter().map(|e| {
                            let marker = match &e.status {
                                spur_acp::PlanEntryStatus::Completed => "[x]",
                                spur_acp::PlanEntryStatus::InProgress => "[~]",
                                _ => "[ ]",
                            };
                            format!("{} {}", marker, e.content)
                        }).collect::<Vec<_>>().join("\n");
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Think,
                            text,
                            timestamp: Self::now_stamp(),
                            #[cfg(feature = "markdown")]
                            markdown: None,
                        });
                    }
                    _ => {}
                }
            }

            SpurEventBody::DelegationRequested {
                from,
                to_agent,
                task,
                request_id,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
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
                // Update the most recent delegate entry that matches this worker.
                // Since we don't have a direct session→agent mapping here, update
                // the last delegate entry with an active status.
                let _ = worker_session; // avoid unused warning
                let status_str = match status {
                    DelegationStatus::Success => "done",
                    DelegationStatus::Failed { .. } => "failed",
                    DelegationStatus::Conflict { .. } => "conflict",
                    DelegationStatus::Timeout => "timeout",
                    DelegationStatus::Rejected { .. } => "rejected",
                    DelegationStatus::Modified { .. } => "modified",
                    DelegationStatus::TimedOut { .. } => "timed out",
                    _ => {
                        tracing::warn!("unknown DelegationStatus variant in session_detail status string — update needed");
                        "unknown"
                    }
                };
                // This is a best-effort update; walk entries in reverse to find
                // the most recent active delegation.
                // Note: ReactTrace doesn't expose entries mutably, so we just
                // push a new entry noting the completion instead.
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Think,
                    text: format!("Delegation completed: {}", status_str),
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
                    #[cfg(feature = "markdown")]
                    {
                        use crate::components::markdown_stream::StateLookup;

                        let (error_ids, pending_ids) = self.build_state_lookup_sets();
                        let states = StateLookup { errors: &error_ids, pending: &pending_ids };
                        for (_entry_idx, fence) in self.react_trace.force_flush_all(&states) {
                            self.mermaid_registry.insert(
                                fence.id,
                                crate::components::mermaid::MermaidState::Pending { code: fence.code.clone() },
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
                        kind: TraceKind::Observe,
                        text: format!("BRAIN ERROR: {}", message),
                        timestamp: Self::now_stamp(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
            }

            SpurEventBody::AgentExtNotification { session, method, params } => {
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
                ..
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
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
            let states = StateLookup { errors: &error_ids, pending: &pending_ids };

            for (_entry_idx, fence) in self.react_trace.drain_fence_dispatches(&states) {
                self.mermaid_registry.insert(
                    fence.id,
                    crate::components::mermaid::MermaidState::Pending { code: fence.code.clone() },
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

    fn render(&self, frame: &mut Frame, area: Rect) {
        debug_assert!(
            false,
            "SessionDetailView::render called via trait \u{2014} use render_with_lineage instead"
        );
        self.render_inner(frame, area, None);
    }
}
impl SessionDetailView {
    fn render_inner(
        &self,
        frame: &mut Frame,
        area: Rect,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
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
            let [banner, content] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(area_rest);
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

        let input_height = self.input_bar.required_height();
        let chunks = Layout::vertical([
            Constraint::Length(1),            // header
            Constraint::Min(4),              // react trace (fills)
            Constraint::Length(input_height), // input bar
            Constraint::Length(1),            // status bar
        ])
        .split(content_area);

        // ── Header: breadcrumb + elapsed + cost ─────────────────────────
        let header = Line::from(vec![
            Span::styled(
                " Dashboard > ",
                Style::default().fg(Color::DarkGray),
            ),
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
            self.react_trace.render_with_ctx(frame, chunks[1], &ctx, lineage);
        }
        #[cfg(not(feature = "markdown"))]
        self.react_trace.render(frame, chunks[1], lineage);

        // ── Input bar ───────────────────────────────────────────────────
        self.input_bar.render(frame, chunks[2]);

        // ── Completion popup (overlay above the InputBar) ──────────────
        if self.popup_open() {
            self.completion_popup
                .borrow_mut()
                .render(frame, chunks[2], area);
        }

        // ── Status bar ──────────────────────────────────────────────────
        StatusBar::render(
            frame,
            chunks[3],
            StatusBarProps {
                view: &ViewId::SessionDetail(self.session_id.clone()),
                running: 0,
                pending_review: 0,
                total_cost: self.cost,
                elapsed: &elapsed,
                current_mode: self.current_mode.as_deref(),
                context_used: self.context_used,
                context_size: self.context_size,
            },
        );

        // ── Resume banner (top row, if visible) ─────────────────────────
        if let (Some(banner), Some(rect)) =
            (self.resume_banner.as_ref(), resume_banner_area)
        {
            banner.render(frame, rect);
        }
    }

    /// Render with an `ExecutorLineage` reference so inline executor
    /// cards can splice into the conversation at Delegate entries.
    /// Called from `app.rs` in place of the trait `render`.
    pub fn render_with_lineage(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        lineage: &spur_core::lineage::projection::ExecutorLineage,
    ) {
        self.render_inner(frame, area, Some(lineage));
    }
}

// ─── Formatting helpers ─────────────────────────────────────────────────

/// Extract the text content from a `ContentChunk`, if it contains text.
fn extract_text(chunk: &spur_acp::ContentChunk) -> Option<&str> {
    match &chunk.content {
        spur_acp::ContentBlock::Text(tc) => Some(&tc.text),
        _ => None,
    }
}

/// Format tool call args for display. Extracts purpose or key args,
/// falls back to truncated JSON.
fn format_tool_args(input: &serde_json::Value) -> String {
    if input.is_null() {
        return String::new();
    }
    if let Some(obj) = input.as_object() {
        if obj.is_empty() {
            return String::new();
        }
        // Kiro includes __tool_use_purpose — use it if available
        if let Some(purpose) = obj.get("__tool_use_purpose").and_then(|v| v.as_str()) {
            return purpose.to_string();
        }
        // Try common meaningful keys
        for key in &["path", "file", "command", "query", "url", "pattern"] {
            if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
                return format!("{}: {}", key, val);
            }
        }
    }
    // Fallback: truncate JSON to single line
    let s = input.to_string();
    truncate_str(&s, 80)
}

/// Format tool result output for display. Truncates to 3 lines.
fn format_observe_output(output: &serde_json::Value) -> String {
    if output.is_null() {
        return "[no output]".to_string();
    }
    // If it's a simple string, use directly
    if let Some(text) = output.as_str() {
        return truncate_lines(text, 3);
    }
    // Extract text from ACP wrapper: {"items":[{"Text":"..."}, ...]}
    if let Some(items) = output.get("items").and_then(|v| v.as_array()) {
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|item| {
                item.get("Text")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("text").and_then(|v| v.as_str()))
            })
            .collect();
        if !texts.is_empty() {
            let joined = texts.join("\n");
            return truncate_lines(&joined, 3);
        }
    }
    // Fallback: stringify JSON
    truncate_lines(&output.to_string(), 3)
}

/// Truncate a string to max_len chars, respecting UTF-8 boundaries.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Truncate text to max_lines, showing a count of remaining lines.
fn truncate_lines(s: &str, max_lines: usize) -> String {
    let total = s.lines().count();
    if total <= max_lines {
        return s.to_string();
    }
    let preview: String = s.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    format!("{}\n... [{} more lines]", preview, total - max_lines)
}

#[cfg(all(test, feature = "markdown"))]
mod invalidate_protocols_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use image::{DynamicImage, RgbaImage};
    use std::cell::RefCell;

    fn test_view() -> SessionDetailView {
        SessionDetailView::new(
            spur_acp::SessionId("test".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
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
        if let Some(MermaidState::Ready { inline_protocol, .. }) =
            view.mermaid_registry.get(&id)
        {
            assert!(inline_protocol.borrow().is_none());
        }

        // Invalidate is a no-op on already-None slots but must not panic or
        // mutate other variants.
        view.mermaid_registry
            .insert(MermaidId(2), MermaidState::Rendering);
        view.mermaid_registry.insert(
            MermaidId(3),
            MermaidState::Error { message: "boom".to_string() },
        );
        view.invalidate_inline_protocols();

        assert!(matches!(view.mermaid_registry.get(&MermaidId(1)), Some(MermaidState::Ready { .. })));
        assert!(matches!(view.mermaid_registry.get(&MermaidId(2)), Some(MermaidState::Rendering)));
        assert!(matches!(view.mermaid_registry.get(&MermaidId(3)), Some(MermaidState::Error { .. })));

        if let Some(MermaidState::Ready { inline_protocol, .. }) =
            view.mermaid_registry.get(&MermaidId(1))
        {
            assert!(inline_protocol.borrow().is_none(), "slot should remain None after invalidate");
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
        let action = <SessionDetailView as crate::views::View>::handle_key(&mut view, key);

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
        let action = <SessionDetailView as crate::views::View>::handle_key(&mut view, key);

        match action {
            Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
            other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod static_command_seeding_tests {
    use super::*;
    use spur_acp::{
        AgentConfig, CommandsConfig, DispatchKind, SessionId, StaticCommandDecl,
    };
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
        );
        let names: Vec<_> = view
            .command_registry
            .list()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert!(names.contains(&"compact".to_string()), "static /compact should be visible at startup, got {names:?}");
    }
}
