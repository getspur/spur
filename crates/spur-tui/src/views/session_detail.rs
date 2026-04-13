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
    pub mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    #[cfg(feature = "markdown")]
    pub pending_fence_actions: std::collections::VecDeque<crate::action::Action>,
}

impl SessionDetailView {
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
    ) -> Self {
        Self {
            session_id,
            agent_name,
            role,
            react_trace: ReactTrace::new(),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
            current_mode: None,
            command_registry: crate::commands::CommandRegistry::new(),
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

    /// Merged slash-command registry (spur-local + agent-advertised).
    pub fn command_registry(&self) -> &crate::commands::CommandRegistry {
        &self.command_registry
    }

    /// Lowercase agent identifier used for namespacing commands in the
    /// registry (e.g. `"claude"`, `"kiro"`).
    pub(crate) fn agent_handle_for_commands(&self) -> String {
        self.agent_name.to_lowercase()
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
        let is_error = result.is_err();
        let state = match result {
            // The registry stores owned `DynamicImage`, unwrap the Arc via clone
            // of the underlying data. If the Arc has a single ref, this is O(1);
            // otherwise it copies pixels once (still rare — Arc is typically
            // single-owner at this point).
            Ok(image_arc) => MermaidState::Ready { image: (*image_arc).clone() },
            Err(message) => MermaidState::Error { message },
        };
        self.mermaid_registry.insert(ref_id, state);

        // On error, mark every markdown stream dirty so the next tick's
        // maybe_flush rebuilds placeholders with the error indicator.
        if is_error {
            self.react_trace.mark_all_streams_dirty();
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
}

impl View for SessionDetailView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
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

        #[cfg(feature = "markdown")]
        if matches!(key.code, KeyCode::Char('v')) && key.modifiers.contains(KeyModifiers::ALT) {
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
                        SubmitDecision::KiroExecute { command, args } => {
                            Some(Action::KiroExecute {
                                session: self.session_id.clone(),
                                command,
                                args,
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
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Delegate {
                        agent: to_agent.clone(),
                        task: task.clone(),
                        status: "delegated".to_string(),
                    },
                    text: String::new(),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
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
                    // No trace entry — the InputBar status indicator shows "ready".
                    // A separator is enough to visually divide turns.
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
                if method == spur_acp::ext::KIRO_COMMANDS_AVAILABLE {
                    if let Some(arr) = params.get("availableCommands").cloned() {
                        if let Ok(parsed) =
                            serde_json::from_value::<Vec<spur_acp::AvailableCommand>>(arr)
                        {
                            self.command_registry.set_agent_commands("kiro", parsed);
                        }
                    }
                } else if method == spur_acp::ext::SPUR_KIRO_EXECUTE_RESPONSE {
                    self.push_system_note(format!(
                        "\u{27e8}kiro\u{27e9} response: {}",
                        params
                    ));
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
            use crate::components::mermaid::MermaidState;
            use crate::components::markdown_stream::StateLookup;

            // Build the set of fence ids currently in error state so that
            // rebuilt placeholders reflect error vs pending.
            let error_ids: std::collections::HashSet<crate::components::mermaid::MermaidId> =
                self.mermaid_registry
                    .iter()
                    .filter_map(|(id, state)| {
                        if matches!(state, MermaidState::Error { .. }) {
                            Some(*id)
                        } else {
                            None
                        }
                    })
                    .collect();
            let states = StateLookup { errors: &error_ids };

            for (_entry_idx, fence) in self.react_trace.drain_fence_dispatches(&states) {
                self.mermaid_registry.insert(
                    fence.id,
                    MermaidState::Pending { code: fence.code.clone() },
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
        let elapsed = self.elapsed();

        // If an auth error is active, split off the top 3 rows for a red
        // banner. This preserves the rest of the layout exactly as before.
        let (banner_area, content_area) = if self.auth_error.is_some() {
            let [banner, content] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(area);
            (Some(banner), content)
        } else {
            (None, area)
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
        self.react_trace.render(frame, chunks[1]);

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
