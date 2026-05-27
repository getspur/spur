use super::*;

impl App {
    pub(super) fn process_session_lifecycle(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::SendMessage {
                mut session,
                blocks,
                interrupt,
            } => {
                // Plan C Tier 2 - MVP gate-check site for the upgrade
                // modal. `Action::SendMessage` is the dominant interactive
                // command-execution path in the TUI (every prompt to an
                // attached brain flows through it), making it the natural
                // counterpart to the CLI's `spur exec` denial path that
                // Tier 1 wired into stderr.
                //
                // `cli_core_exec` is community-tier in the embedded
                // policy, so production users will not normally hit this
                // branch - the MVP demo path is
                // `SPUR_LICENSE_TEST_STRIP_KEYS=cli_core_exec`, mirroring
                // the binary smoke pattern from Tier 1.
                if let Err(err) = spur_license::require_feature(
                    &self.feature_gate,
                    spur_license::FeatureKey::CLI_CORE_EXEC,
                ) {
                    let required_tier = spur_license::upgrade_cta::required_tier_for(
                        spur_license::FeatureKey::CLI_CORE_EXEC,
                    );
                    return Some(Action::ShowUpgradeModal { err, required_tier });
                }

                // Empty session means "route to the currently active session".
                // Dashboard's InputBar emits this when a brain is attached.
                if session.0.is_empty() {
                    if let Some(ref detail) = self.session_detail {
                        session = detail.session_id().clone();
                    } else {
                        tracing::warn!(
                            "SendMessage with empty session and no active session_detail - \
                             dropping (caller should have used NewSessionWithMessage)"
                        );
                        return None;
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
                        "SendMessage: session_detail is None - no local echo (orchestrator owns the prompt)"
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
                None
            }

            Action::ClearSession => {
                self.pending_first_user_message = None;
                // /clear is a spur-local META command. Spec section 3.6 requires
                // send-first ordering: if the channel send fails, the brain is
                // NOT retired, so we must NOT visually reset the view -
                // otherwise the user sees "cleared" while the stale brain is
                // still active (ghost-cleared state).
                self.close_active_notebook_daemon();
                let send_ok = match self.user_input_tx.as_ref() {
                    Some(tx) => match tx.try_send(UserInput::NewSessionWithMessage {
                        blocks: vec![],
                        interrupt: false,
                    }) {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::error!(
                                err = ?e,
                                "Action::ClearSession: user_input tx send failed - \
                                 brain NOT retired; view NOT reset to avoid ghost-cleared state"
                            );
                            false
                        }
                    },
                    None => {
                        tracing::error!(
                            "Action::ClearSession: user_input_tx is None; \
                             cannot retire brain - view NOT reset"
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
                None
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
                None
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
                self.navigate_to(ViewId::SessionPicker);
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ListSessions);
                }
                None
            }

            Action::ResumeSession { session_id } => {
                self.pending_first_user_message = None;
                // Optimistic navigation: move to SessionDetail immediately so
                // the picker dismisses in the same tick (FP-6). Lazy-construct
                // a pre-ready SessionDetailView so LoadState renders correctly
                // while the resume pipeline is in flight (Tranche 2 Task 5).
                self.close_active_notebook_daemon();
                let sid = SessionId(session_id.clone());
                let view =
                    crate::views::session_detail::SessionDetailView::for_session(sid.clone());
                self.session_detail = Some(view);
                self.navigate_to(ViewId::SessionDetail(sid));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumeSession { session_id });
                }
                None
            }

            Action::RefreshSessions => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::ListSessions);
                }
                self.dirty = true;
                None
            }

            Action::CancelStream { session } => {
                tracing::debug!(session = %session.0, "dispatching CancelStream to orchestrator");
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::CancelStream { session });
                }
                None
            }

            Action::CopySessionId(session_id) => {
                use base64::{engine::general_purpose::STANDARD, Engine};
                use std::io::Write;
                let payload = STANDARD.encode(session_id.as_bytes());
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{payload}\x1b\\");
                let _ = out.flush();
                tracing::debug!(target: "spur_tui::picker", session_id = %session_id, "OSC 52 copy emitted");
                None
            }

            Action::NewSessionRequested => {
                // Retire the current brain AND eagerly spawn a fresh session so
                // the user lands directly on the new SessionDetail view (via the
                // BrainSpawned auto-navigate at events.rs). Distinct from
                // ClearSession, which uses NewSessionWithMessage{empty} to defer
                // spawn until the next Message - that path preserves the open
                // SessionDetail for the `/clear` reset banner.
                self.close_active_notebook_daemon();
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::NewSession);
                }
                None
            }

            _ => None,
        }
    }

    fn close_active_notebook_daemon(&self) {
        let Some(session_id) = self
            .session_detail
            .as_ref()
            .map(|detail| detail.session_id().clone())
        else {
            return;
        };
        let Some(socket_nonce) = self.notebook_socket_nonces.get(&session_id.0).cloned() else {
            return;
        };
        let socket_path = spur_core::notebook::control_socket_path(&socket_nonce);
        tokio::spawn(async move {
            match crate::notebook_daemon::send_notebook_command("close", &socket_path).await {
                Ok(response) if response.ok => {
                    tracing::debug!(
                        path = response.path.as_deref(),
                        socket = %socket_path.display(),
                        "notebook daemon close command completed during session switch"
                    );
                }
                Ok(response) => {
                    if let Some(error) = response.error.as_ref() {
                        tracing::debug!(
                            code = %error.code,
                            message = %error.message,
                            "notebook daemon close command failed during session switch"
                        );
                    } else {
                        tracing::debug!(
                            "notebook daemon close command failed during session switch"
                        );
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        %error,
                        "notebook daemon close command failed during session switch"
                    );
                }
            }
        });
    }
}
