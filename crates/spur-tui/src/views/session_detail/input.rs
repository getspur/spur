use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{Action, ViewId};
use crate::components::input_bar::ActivityKind;

use super::{FocusedSessionPanel, SessionDetailView};

impl SessionDetailView {
    pub(super) fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
        // ── macOS Option-key normalisation ─────────────────────────────
        // macOS terminals send Unicode characters (e.g. `∑` for Option-W)
        // instead of Alt escape sequences when "Use Option as Meta key" is
        // off (the default).  Map the most common US-QWERTY Option-letter
        // characters back to Alt+<ascii> so the keybindings work
        // out-of-the-box.
        let key = super::super::normalize_macos_option(key);

        // Dismiss the auth banner on any keystroke (before any further routing).
        // The mode-toggle binding below still fires because the action is
        // dispatched regardless.
        if self.auth_error.is_some() {
            self.auth_error = None;
        }

        // Priority 0a: confirmation modal — when open, captures all keys
        // until the user makes an explicit yes/no choice or dismisses.
        //
        // Vim-safe dismissal: a reflexive double-tap of `Esc` (Insert→Normal
        // habit) opens then closes the modal with no destructive action.
        // `Enter` is intentionally NOT a confirmation key — vim users press
        // Enter to commit Normal-mode commands, and we do not want that
        // muscle-memory to cancel an in-flight turn.
        if self.cancel_confirm_open {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.cancel_confirm_open = false;
                    self.cancelling_in_flight = true;
                    self.cancel_hint_until = Some(Instant::now() + Duration::from_secs(2));
                    self.push_cancel_note();
                    self.input_bar.set_status(
                        Some(format!("[{}: cancelling{{spinner}}]", self.agent_name)),
                        ActivityKind::Cancelling,
                    );
                    return Some(Action::CancelStream {
                        session: self.session_id.clone(),
                    });
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_confirm_open = false;
                    return None;
                }
                _ => return None,
            }
        }

        // Priority 0b: Esc-to-cancel takes precedence when a stream is in flight
        // and we're not already cancelling. Opens the confirmation modal
        // instead of dispatching `CancelStream` directly — vim users would
        // otherwise lose work to a single reflexive `Esc`. Second Esc (after
        // dispatch, when `cancelling_in_flight == true`) falls through to the
        // existing Esc handlers (popup dismiss / NavigateBack).
        // Exception: in Vim Insert/Visual mode, Esc first exits to Normal mode.
        if matches!(key.code, KeyCode::Esc)
            && self.stream_in_flight
            && !self.cancelling_in_flight
            && !self.input_bar.wants_esc()
        {
            self.cancel_confirm_open = true;
            return None;
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
                        KeyCode::Up
                            | KeyCode::Down
                            | KeyCode::Esc
                            | KeyCode::Tab
                            | KeyCode::BackTab
                            | KeyCode::Enter
                    ) || ((key.code == KeyCode::Char('c')
                        || key.code == KeyCode::Char('p')
                        || key.code == KeyCode::Char('n'))
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                } else {
                    true
                };
                if shell_consumes {
                    if self.input_bar.paste_burst_active() && matches!(key.code, KeyCode::Enter) {
                        KeyOwner::Composer
                    } else {
                        KeyOwner::Picker
                    }
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
                    let is_tab_owned_by_composer = !self.input_bar.is_empty()
                        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab);
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
                    ) || is_tab_owned_by_composer
                        || (key.code == KeyCode::Esc && self.input_bar.wants_esc()))
                        && !is_permission_key;

                    if is_composer_editing {
                        // Empty-bar nav chars (j/k/g/G) and Up/Down/Esc are
                        // View-owned scroll/nav keys — no rescue block needed.
                        if self.input_bar.is_empty()
                            && (matches!(key.code, KeyCode::Char('j' | 'k' | 'g' | 'G'))
                                || (matches!(key.code, KeyCode::Char('?'))
                                    && key.modifiers.is_empty())
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
            KeyOwner::Picker => self
                .completion
                .handle_picker_key(key, &mut self.input_bar)
                .and_then(|accept| {
                    crate::commands::submit_router::local_action_from_picker_accept(
                        accept,
                        &self.command_registry,
                        self.spur_agent_caps.as_deref(),
                    )
                }),

            KeyOwner::Composer => {
                use crate::components::completion_trigger::IntentEvent;
                use crate::components::input_bar::HandleOutcome;
                match self.input_bar.handle_key(key) {
                    HandleOutcome::Submit(_, _) => {
                        if let Some(ref mut banner) = self.resume_banner {
                            banner.record_message_sent();
                        }
                        self.dispatch_intent(IntentEvent::Submitted);
                        let pending_images = self.input_bar.take_pending_images();
                        if let Some((text, ranges, interrupt)) =
                            self.input_bar.take_submit_capture()
                        {
                            use crate::commands::submit_router::{route_with_caps, SubmitDecision};
                            let dec = route_with_caps(
                                &text,
                                &ranges,
                                &pending_images,
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
                                    let has_code_mentions = ranges
                                        .iter()
                                        .any(|range| range.uri.starts_with("graph://"));
                                    let has_datasource_mentions = ranges
                                        .iter()
                                        .any(|range| range.uri.starts_with("datasource://"));
                                    if has_code_mentions {
                                        let mut mention_registry =
                                            self.mention_registry.borrow_mut();
                                        mention_registry.retain_code_payloads_for_uris(
                                            ranges.iter().map(|range| range.uri.as_str()),
                                        );
                                        blocks = crate::commands::submit_router::assemble_blocks_with_code_mentions(
                                            &text,
                                            &ranges,
                                            &pending_images,
                                            &self.cwd,
                                            |uri| mention_registry.lookup_code_payload(uri),
                                        );
                                    }
                                    if has_datasource_mentions {
                                        let mention_registry = self.mention_registry.borrow();
                                        let _ = crate::mentions::hint::prepend_datasource_hint(
                                            &mut blocks,
                                            &ranges,
                                            |uri| {
                                                mention_registry
                                                    .lookup_datasource_hint(uri)
                                                    .map(str::to_string)
                                            },
                                        );
                                    }
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
                                        self.pending_model_override = Some(value.clone());
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
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) && self.input_bar.is_empty()
                {
                    self.focused_panel = match self.focused_panel {
                        FocusedSessionPanel::ReactTrace => FocusedSessionPanel::Workers,
                        FocusedSessionPanel::Workers => FocusedSessionPanel::ReactTrace,
                    };
                    return Some(Action::CycleFocus);
                }

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
                        KeyCode::Char('?') if key.modifiers.is_empty() => {
                            return Some(Action::ShowHelp);
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
                            self.cancel_hint_until = None;
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
