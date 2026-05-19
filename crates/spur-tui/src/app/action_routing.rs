use super::*;

impl App {
    fn build_execute_prompt(&self, id: &str) -> String {
        let issue = self
            .dashboard
            .tracked_issues()
            .iter()
            .find(|issue| issue.id == id);

        // Removed constraint: any issue type can now be executed.

        let (title, status, pri, issue_type) = issue
            .map(|issue| {
                (
                    issue.title.clone(),
                    issue.status.clone(),
                    issue
                        .priority
                        .map(|p| format!("P{}", p))
                        .unwrap_or_default(),
                    issue.issue_type.clone().unwrap_or_else(|| "task".into()),
                )
            })
            .unwrap_or_else(|| {
                (
                    String::new(),
                    "unknown".into(),
                    String::new(),
                    "unknown".into(),
                )
            });

        format!(
            "The user wants to execute this work item.\n\n\
             Item: {} \u{2014} {}\n\
             Type: {} | Status: {} | Priority: {}\n\n\
             Please analyze this item, gather necessary information, and determine how to execute it. \
             If it requires a multi-step plan, use the appropriate tools to structure it. \
             If it's a single task, figure out the best way to get it done.",
            id, title, issue_type, status, pri,
        )
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
                    self.navigate_to(ViewId::SessionDetail(session_id.clone()));
                }
                // If no session_detail exists (no brain spawned), ignore.
            }

            Action::NavigateTo(ViewId::Dashboard) => {
                // navigate_to(Dashboard) clears view_history (canonical root).
                // session_detail kept alive (same as NavigateBack).
                self.navigate_to(ViewId::Dashboard);
            }

            Action::NavigateTo(ViewId::SessionPicker) => {
                self.navigate_to(ViewId::SessionPicker);
            }

            Action::NavigateTo(ViewId::PlanInspector(ref session)) => {
                self.plan_inspector = Some(PlanInspectorView::new(session.clone()));
                self.navigate_to(ViewId::PlanInspector(session.clone()));
            }

            Action::InspectPlan {
                ref session_id,
                ref plan_id,
            } => {
                if self.plan_projection.plan(plan_id).is_none() {
                    if let Some(ref tx) = self.user_input_tx {
                        let _ = tx.try_send(UserInput::InspectPlan {
                            plan_id: plan_id.clone(),
                        });
                    }
                }
                self.plan_inspector = Some(PlanInspectorView::new_for_plan(
                    session_id.clone(),
                    plan_id.clone(),
                ));
                self.navigate_to(ViewId::PlanInspector(session_id.clone()));
            }

            Action::OpenPlanInBrowser { ref plan_id } => {
                let Some(current_session) = self
                    .session_detail
                    .as_ref()
                    .map(|detail| detail.session_id().clone())
                else {
                    return self.process_action(Action::FlashHint {
                        message: "Select a brain session first (S)".into(),
                    });
                };
                let just_created = self.plan_browser.is_none();
                let mut session_changed = false;
                if self.plan_browser.is_none() {
                    self.plan_browser = Some(PlanBrowserView::new(current_session.clone()));
                }
                if let Some(browser) = self.plan_browser.as_mut() {
                    session_changed = browser.set_current_session(current_session);
                    browser.focus_plan_id(plan_id.clone());
                }
                self.navigate_to(ViewId::PlanBrowser);
                if just_created || session_changed {
                    self.process_action(Action::RefreshPlans);
                }
            }

            Action::NavigateTo(ViewId::PlanBrowser) => {
                // Inc 1 (bd-d587.1): without an active brain session, no plan can ever
                // classify as Mine (every row is Unowned/Other), so Inspect/Resume have
                // no actionable target. Block-with-hint instead of opening an empty browser.
                let Some(current_session) = self
                    .session_detail
                    .as_ref()
                    .map(|detail| detail.session_id().clone())
                else {
                    return self.process_action(Action::FlashHint {
                        message: "Select a brain session first (S)".into(),
                    });
                };
                let just_created = self.plan_browser.is_none();
                let mut session_changed = false;
                if self.plan_browser.is_none() {
                    self.plan_browser = Some(PlanBrowserView::new(current_session.clone()));
                }
                if let Some(browser) = self.plan_browser.as_mut() {
                    session_changed = browser.set_current_session(current_session);
                }
                self.navigate_to(ViewId::PlanBrowser);
                if just_created || session_changed {
                    self.process_action(Action::RefreshPlans);
                }
            }

            Action::NavigateTo(ViewId::IssueBrowser) => {
                let just_created = self.issue_browser.is_none();
                if just_created {
                    let mut view = IssueBrowserView::new();
                    view.seed_issues(self.dashboard.tracked_issues().to_vec());
                    self.issue_browser = Some(view);
                }
                self.navigate_to(ViewId::IssueBrowser);
                if just_created {
                    // Background refresh - guarantees user sees fresh data after the
                    // dashboard's startup snapshot. No-op when pm_service is None
                    // (orchestrator returns an error which is logged, not surfaced).
                    self.process_action(Action::RefreshIssues);
                }
            }

            Action::OpenInsights | Action::NavigateTo(ViewId::Insights) => {
                #[cfg(feature = "analytics")]
                self.start_insights_init();
                self.navigate_to(ViewId::Insights);
            }

            #[cfg(feature = "markdown")]
            Action::NavigateTo(ViewId::MermaidOverlay(ref session)) => {
                use crate::views::mermaid_viewer::MermaidViewerView;
                self.mermaid_viewer = Some(MermaidViewerView::new(session.clone()));
                self.navigate_to(ViewId::MermaidOverlay(session.clone()));
            }

            Action::NavigateBack => {
                // Inc 2 (bd-d587.2): pop view_history. When empty, falls back to
                // Dashboard (or to active SessionDetail if leaving Dashboard).
                // Overlay state (PlanInspector, MermaidOverlay) is nulled
                // automatically when leaving those views.
                self.navigate_back();
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

                // Toggle: Dashboard -> SessionDetail, otherwise -> Dashboard with Agents focused.
                if matches!(self.current_view, ViewId::Dashboard) {
                    if let Some(ref detail) = self.session_detail {
                        tracing::info!(
                            session_id = %detail.session_id().0,
                            "InspectWorkers: toggling Dashboard -> SessionDetail"
                        );
                        self.navigate_to(ViewId::SessionDetail(detail.session_id().clone()));
                    } else {
                        tracing::info!("InspectWorkers: no session_detail, staying in Dashboard");
                    }
                    return;
                }
                tracing::info!(
                    current_view = ?self.current_view,
                    "InspectWorkers: navigating to Dashboard"
                );

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
                self.navigate_to(ViewId::Dashboard);
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
                self.navigate_to(ViewId::SessionDetail(sid));
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
                self.navigate_to(ViewId::Dashboard);
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
                // Inc 2 (bd-d587.2): navigate_to(Dashboard) also clears view_history,
                // matching the panic-reset intent of returning to a clean root.
                self.navigate_to(ViewId::Dashboard);
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
                if matches!(&self.current_view, ViewId::IssueBrowser) {
                    if let Some(action) = self
                        .issue_browser
                        .as_mut()
                        .and_then(|view| view.take_pending_action())
                    {
                        self.process_action(action);
                    }
                } else {
                    for _ in 0..n {
                        self.dashboard.agents_tree_mut().select_next(&self.lineage);
                    }
                }
            }
            Action::SelectPrevBy(n) => {
                if matches!(&self.current_view, ViewId::IssueBrowser) {
                    if let Some(action) = self
                        .issue_browser
                        .as_mut()
                        .and_then(|view| view.take_pending_action())
                    {
                        self.process_action(action);
                    }
                } else {
                    for _ in 0..n {
                        self.dashboard.agents_tree_mut().select_prev(&self.lineage);
                    }
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
                    let result =
                        crate::components::mermaid::render_mermaid_hybrid(&code, target_width)
                            .map(|rendered| match rendered {
                                crate::components::mermaid::MermaidRendered::Image(image) => {
                                    crate::components::mermaid::MermaidRenderOutput::Image(
                                        std::sync::Arc::new(image),
                                    )
                                }
                                crate::components::mermaid::MermaidRendered::Text { text } => {
                                    crate::components::mermaid::MermaidRenderOutput::Text(
                                        std::sync::Arc::<str>::from(text),
                                    )
                                }
                            })
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
            | Action::FocusWorkerInDashboard { .. }
            | Action::Tick => {}

            // Issue actions — wired to the PM backend; IssuesPanel not yet implemented.
            Action::RefreshIssues => {
                if let Some(ref mut browser) = self.issue_browser {
                    browser.invalidate_graph_cache();
                }
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshIssues);
                }
            }
            Action::RefreshPlans => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshPlans);
                }
            }
            Action::ClaimPlan { plan_id } => {
                self.flash_hint_short(format!("Claiming plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ClaimPlan { plan_id });
                }
            }
            Action::ForceReclaimPlan { plan_id } => {
                self.flash_hint_short(format!("Force reclaiming plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ForceReclaimPlan { plan_id });
                }
            }
            Action::ResumePlan { plan_id } => {
                // Immediate user feedback: the orchestrator → MCP round-trip can take
                // 1–3s; without this hint the TUI looks frozen and invites double-press.
                self.flash_hint_short(format!("Starting plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumePlan { plan_id });
                }
            }
            Action::OpenIssueInBacklog { id } => {
                let just_created = self.issue_browser.is_none();
                if just_created {
                    let mut view = IssueBrowserView::new();
                    view.seed_issues(self.dashboard.tracked_issues().to_vec());
                    self.issue_browser = Some(view);
                }
                // Inc 3 (bd-d587.3): only caller today is PlanBrowser View-Epic,
                // so pass `FocusGraph` — selects the row in the left pane and
                // arms the detail-fetch handler to flip to Graph mode after
                // both `IssueDetailFetched` and `IssueSubgraphLoaded` arrive.
                let (pending, needs_refresh) = self
                    .issue_browser
                    .as_mut()
                    .map(|browser| {
                        browser.open_external_detail(
                            id.clone(),
                            crate::views::issue_browser::OpenMode::FocusGraph,
                        );
                        (browser.take_pending_action(), browser.has_pending_select())
                    })
                    .unwrap_or((None, false));
                self.navigate_to(ViewId::IssueBrowser);
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::GetIssueDetail { id });
                    // Refresh on first creation OR when the requested id
                    // wasn't found in the cached list — otherwise pending_select
                    // would sit forever and the left-pane row would never align
                    // with the right-pane detail.
                    if just_created || needs_refresh {
                        let _ = tx.try_send(UserInput::RefreshIssues);
                    }
                }
                if let Some(action) = pending {
                    self.process_action(action);
                }
            }
            Action::GetIssueGraph { id } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::GetIssueGraph { id });
                }
            }
            Action::FlashHint { message } => {
                self.flash_hint_short(message);
            }
            Action::ThemeCommand { arg } => {
                self.handle_theme_command(arg);
            }
            Action::PrefillInput { text } => {
                match &self.current_view {
                    ViewId::Dashboard => {
                        self.dashboard.prefill_input(text);
                    }
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.prefill_input(text);
                        }
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            Action::Issue(issue_action) => {
                match issue_action {
                    crate::action::IssueAction::ViewDetail { id } => {
                        if let Some(ref tx) = self.user_input_tx {
                            // PROBE: issue_detail_latency
                            tracing::info!(
                                target: "issue_probe",
                                site = "ui_send",
                                id = %id,
                                queue_len = tx.capacity().saturating_sub(tx.max_capacity()),
                                ts_ns = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_nanos() as u64)
                                    .unwrap_or(0),
                                "GetIssueDetail dispatched from TUI",
                            );
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
                    crate::action::IssueAction::Execute { id } => {
                        let prompt = self.build_execute_prompt(&id);

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
                    crate::action::IssueAction::ExecuteEdit { id } => {
                        let prompt = self.build_execute_prompt(&id);

                        if let Some(session_id) = self
                            .session_detail
                            .as_ref()
                            .map(|detail| detail.session_id().clone())
                        {
                            self.process_action(Action::NavigateTo(ViewId::SessionDetail(
                                session_id,
                            )));
                        } else {
                            self.process_action(Action::NavigateTo(ViewId::Dashboard));
                        }

                        self.process_action(Action::PrefillInput { text: prompt });
                        self.process_action(Action::FlashHint {
                            message: EXECUTE_EDIT_HINT.to_string(),
                        });
                    }
                    crate::action::IssueAction::AddComment { issue_id, body } => {
                        if let Some(ref tx) = self.user_input_tx {
                            let _ = tx.try_send(UserInput::AddIssueComment { issue_id, body });
                        }
                    }
                }
            }
        }
    }

    pub(super) fn handle_permission_request(
        &mut self,
        request: spur_acp::types::PermissionRequest,
    ) {
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
    pub(super) fn clear_pending_permission_trace(&mut self) {
        if let Some(ref mut detail) = self.session_detail {
            detail.resolve_pending_permissions();
        }
    }
}

fn to_wire_decision(d: &spur_core::ReviewDecision) -> spur_acp::ReviewDecision {
    d.clone()
}
